// src/clipboard.rs
//
// System clipboard and primary selection.
//
// On Wayland the window's own connection is used (smithay-clipboard), so
// Cyberterm offers selections as the focused client, which every
// compositor honours. arboard is the fallback (X11, macOS, or before the
// window exists): on Wayland it goes through the data-control protocol,
// where Hyprland doesn't keep a primary selection set that way.

#[cfg(not(target_os = "android"))]
use arboard::Clipboard;
#[cfg(target_os = "android")]
use jni::{
    jni_sig, jni_str,
    objects::{JObject, JString, JValue},
    Env, JavaVM,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Clipboard,
    /// The X11/Wayland primary selection: whatever was last selected,
    /// pasted with middle-click. Falls back to the clipboard on platforms
    /// without one.
    Primary,
}

pub struct ClipboardManager {
    #[cfg(target_os = "linux")]
    wayland: Option<smithay_clipboard::Clipboard>,
    #[cfg(not(target_os = "android"))]
    fallback: Option<Clipboard>,
    #[cfg(target_os = "android")]
    android: ndk_context::AndroidContext,
}

/// Box-drawing characters copied out of TUIs are swapped for ASCII so a
/// pasted table still lines up in places without those glyphs.
fn clean_box_drawing(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '─' | '━' | '═' => '-',
            '│' | '┃' | '║' => '|',
            '┌' | '┐' | '└' | '┘' | '├' | '┤' | '┬' | '┴' | '┼' | '╭' | '╮' | '╯' | '╰' => {
                '+'
            }
            other => other,
        })
        .collect()
}

impl ClipboardManager {
    /// Always succeeds; operations are silently unavailable when no
    /// backend works (a terminal is fully usable without a clipboard).
    pub fn new() -> Self {
        Self {
            #[cfg(target_os = "linux")]
            wayland: None,
            #[cfg(not(target_os = "android"))]
            fallback: Clipboard::new().ok(),
            #[cfg(target_os = "android")]
            android: ndk_context::android_context(),
        }
    }

    /// Switches to the window's Wayland connection when there is one.
    pub fn attach(&mut self, window: &winit::window::Window) {
        #[cfg(target_os = "linux")]
        {
            use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
            if let Ok(handle) = window.display_handle() {
                if let RawDisplayHandle::Wayland(wayland) = handle.as_raw() {
                    // SAFETY: the display pointer comes from the live
                    // window, and `App` drops this clipboard before the
                    // window and event loop (field order in `App`).
                    self.wayland = Some(unsafe {
                        smithay_clipboard::Clipboard::new(wayland.display.as_ptr())
                    });
                }
            }
        }
        let _ = window;
    }

    pub fn set(&mut self, kind: Kind, text: &str, clean: bool) {
        let text = if clean {
            clean_box_drawing(text)
        } else {
            text.to_string()
        };
        #[cfg(target_os = "linux")]
        {
            if let Some(wayland) = &self.wayland {
                match kind {
                    Kind::Clipboard => wayland.store(text),
                    Kind::Primary => wayland.store_primary(text),
                }
                return;
            }
            if kind == Kind::Primary {
                use arboard::{LinuxClipboardKind, SetExtLinux};
                if let Some(fallback) = &mut self.fallback {
                    if let Err(e) = fallback
                        .set()
                        .clipboard(LinuxClipboardKind::Primary)
                        .text(text)
                    {
                        eprintln!("cyberterm: primary selection not set: {e}");
                    }
                }
                return;
            }
        }
        let _ = kind;
        #[cfg(target_os = "android")]
        if self.android_set_text(&text).is_none() {
            eprintln!("cyberterm: Android clipboard write failed");
        }
        #[cfg(not(target_os = "android"))]
        if let Some(fallback) = &mut self.fallback {
            if let Err(e) = fallback.set_text(text) {
                eprintln!("cyberterm: clipboard not set: {e}");
            }
        }
    }

    pub fn get(&mut self, kind: Kind) -> Option<String> {
        #[cfg(target_os = "linux")]
        {
            if let Some(wayland) = &self.wayland {
                return match kind {
                    Kind::Clipboard => wayland.load(),
                    Kind::Primary => wayland.load_primary(),
                }
                .ok();
            }
            if kind == Kind::Primary {
                use arboard::{GetExtLinux, LinuxClipboardKind};
                return self
                    .fallback
                    .as_mut()?
                    .get()
                    .clipboard(LinuxClipboardKind::Primary)
                    .text()
                    .ok();
            }
        }
        let _ = kind;
        #[cfg(not(target_os = "android"))]
        {
            self.fallback.as_mut()?.get_text().ok()
        }
        #[cfg(target_os = "android")]
        {
            self.android_get_text()
        }
    }

    #[cfg(target_os = "android")]
    fn with_android_env<T>(
        &self,
        operation: impl for<'local> FnOnce(&mut Env<'local>, &JObject<'local>) -> jni::errors::Result<T>,
    ) -> Option<T> {
        let vm = unsafe { JavaVM::from_raw(self.android.vm().cast()) };
        vm.attach_current_thread(|env| {
            // ndk-context owns a global reference for the Android process lifetime.
            let global = unsafe { JObject::from_raw(env, self.android.context().cast()) };
            let context = env.new_local_ref(global)?;
            operation(env, &context)
        })
        .ok()
    }

    #[cfg(target_os = "android")]
    fn android_clipboard_service<'a>(
        env: &mut Env<'a>,
        context: &JObject<'a>,
    ) -> jni::errors::Result<JObject<'a>> {
        let name = env.new_string("clipboard")?;
        env.call_method(
            context,
            jni_str!("getSystemService"),
            jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
            &[JValue::Object(name.as_ref())],
        )?
        .l()
    }

    #[cfg(target_os = "android")]
    fn android_set_text(&self, text: &str) -> Option<()> {
        self.with_android_env(|env, context| {
            let label = env.new_string("")?;
            let text = env.new_string(text)?;
            let clip = env
                .call_static_method(
                    jni_str!("android/content/ClipData"),
                    jni_str!("newPlainText"),
                    jni_sig!("(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Landroid/content/ClipData;"),
                    &[
                        JValue::Object(label.as_ref()),
                        JValue::Object(text.as_ref()),
                    ],
                )?
                .l()?;
            let clipboard = Self::android_clipboard_service(env, context)?;
            env.call_method(
                clipboard,
                jni_str!("setPrimaryClip"),
                jni_sig!("(Landroid/content/ClipData;)V"),
                &[JValue::Object(&clip)],
            )?;
            Ok(())
        })
    }

    #[cfg(target_os = "android")]
    fn android_get_text(&self) -> Option<String> {
        self.with_android_env(|env, context| {
            let clipboard = Self::android_clipboard_service(env, context)?;
            let clip = env
                .call_method(
                    clipboard,
                    jni_str!("getPrimaryClip"),
                    jni_sig!("()Landroid/content/ClipData;"),
                    &[],
                )?
                .l()?;
            if clip.is_null() {
                return Ok(None);
            }
            let item = env
                .call_method(
                    clip,
                    jni_str!("getItemAt"),
                    jni_sig!("(I)Landroid/content/ClipData$Item;"),
                    &[JValue::Int(0)],
                )?
                .l()?;
            let text = env
                .call_method(
                    item,
                    jni_str!("coerceToText"),
                    jni_sig!("(Landroid/content/Context;)Ljava/lang/CharSequence;"),
                    &[JValue::Object(context)],
                )?
                .l()?;
            if text.is_null() {
                return Ok(None);
            }
            let value = env
                .call_method(
                    text,
                    jni_str!("toString"),
                    jni_sig!("()Ljava/lang/String;"),
                    &[],
                )?
                .l()?;
            let value = env.cast_local::<JString>(value)?;
            let copied = value.mutf8_chars(env)?.to_string();
            Ok(Some(copied))
        })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_drawing_becomes_ascii() {
        assert_eq!(clean_box_drawing("┌─┐\n│x│"), "+-+\n|x|");
    }
}
