// Cyberterm - Fully customizable terminal written in rust.
// The ultimate, scriptable, vintage-meets-modern terminal engine.

mod agent;
mod agent_home;
mod agent_merge;
mod agent_setup;
mod ai;
mod app;
mod blocks;
mod boxdraw;
mod changes;
mod cli;
mod clipboard;
mod config;
mod control;
mod danger;
mod find;
mod flight_log;
mod frame;
mod fuzzy;
mod graphics;
mod history;
mod input;
mod json_viewer;
mod layout;
mod layout_file;
mod mcp;
mod mux;
mod popular_themes;
mod procs;
mod renderer;
mod renderer_images;
mod rewind;
mod session;
mod shell;
mod theme;
mod theme_install;
mod tty_client;
mod ui;

use std::path::PathBuf;

use theme::ThemeRegistry;
use winit::event_loop::{EventLoop, EventLoopBuilder};

#[cfg(target_os = "android")]
use winit::platform::android::EventLoopBuilderExtAndroid;

/// Launch Cyberterm with the native desktop event loop.
#[cfg(not(target_os = "android"))]
pub fn run_desktop() {
    run_inner(|_| {});
}

/// Launch Cyberterm inside the Android NativeActivity provided by the app.
#[cfg(target_os = "android")]
pub fn run_android(app: winit::platform::android::activity::AndroidApp) {
    let Some(home) = app.internal_data_path() else {
        eprintln!("cyberterm: Android did not provide private app storage");
        return;
    };
    if let Err(error) = std::fs::create_dir_all(&home) {
        eprintln!("cyberterm: cannot initialize app storage: {error}");
        return;
    }
    std::env::set_var("HOME", &home);
    std::env::set_var("USER", "cyberterm");
    std::env::set_var("SHELL", "/system/bin/sh");
    std::env::set_var("TERM", "xterm-256color");
    run_inner(move |builder| {
        builder.with_android_app(app);
    });
}

fn run_inner(configure_event_loop: impl FnOnce(&mut EventLoopBuilder<session::UserEvent>)) {
    // Agent hooks run on every desktop tool call: skip normal startup.
    #[cfg(not(target_os = "android"))]
    {
        let mut args = std::env::args();
        if args.nth(1).as_deref() == Some("+hook") {
            flight_log::run_hook(&args.collect::<Vec<_>>());
            return;
        }
    }
    let themes_dir = match config::initialize_cyberterm_directories() {
        Ok(base_path) => base_path.join("themes"),
        Err(e) => {
            eprintln!(
                "Initialization Warning: Could not verify data paths ({})",
                e
            );
            PathBuf::from("themes")
        }
    };
    let config_root = themes_dir.parent().unwrap_or(&themes_dir).to_path_buf();

    let cyber_config = match config::load_config(&config_root) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("cyberterm: config not loaded, using defaults: {e}");
            config::CyberConfig::default()
        }
    };

    #[cfg(not(target_os = "android"))]
    let (startup_layout, startup_attach) = {
        let sys_args: Vec<String> = std::env::args().collect();
        match cli::handle_arguments(sys_args, &config_root, cyber_config.clone()) {
            cli::CliAction::ExitCleanly => return,
            cli::CliAction::RunTerminal => (None, None),
            cli::CliAction::RunLayout(path) => {
                (Some(std::fs::canonicalize(&path).unwrap_or(path)), None)
            }
            cli::CliAction::RunAttach(target) => (None, Some(target)),
        }
    };
    #[cfg(target_os = "android")]
    let (startup_layout, startup_attach) = (None, None);
    let mut cyber_config = cyber_config;
    #[cfg(target_os = "android")]
    {
        // The first mobile release owns its shell in the Activity process;
        // the desktop daemon re-execs the CLI binary, which Android has none of.
        cyber_config.daemon.enabled = false;
        cyber_config.control.enabled = false;
    }
    if startup_attach.is_some() {
        // Attaching only makes sense with the daemon.
        cyber_config.daemon.enabled = true;
    }

    // TERM=alacritty when its terminfo is installed (our parser *is*
    // alacritty's, so that entry describes exactly what we support),
    // otherwise xterm-256color; COLORTERM=truecolor either way.
    alacritty_terminal::tty::setup_env();

    let mut registry = ThemeRegistry::load_from_dir(&themes_dir);
    let shared_active = registry.append_cybercore_themes();
    let shared_revision = cybercore::theme::ThemeCatalog::load()
        .map(|catalog| catalog.revision())
        .unwrap_or_default();
    let initial_theme = registry.initial(shared_active.as_deref(), &cyber_config.theme);
    if let Some(index) = initial_theme
        .as_ref()
        .and_then(|t| registry.themes.iter().position(|r| r.name == t.name))
    {
        registry.selected_index = index;
    }

    let mut builder = EventLoop::<session::UserEvent>::with_user_event();
    configure_event_loop(&mut builder);
    let event_loop = match builder.build() {
        Ok(event_loop) => event_loop,
        Err(e) => {
            eprintln!("CRITICAL: cannot connect to the display: {e}");
            std::process::exit(1);
        }
    };
    let mut app = app::App::new(
        event_loop.create_proxy(),
        app::Startup {
            config_root,
            themes_dir,
            config: cyber_config,
            registry,
            initial_theme,
            shared_theme_revision: shared_revision,
            layout: startup_layout,
            attach: startup_attach,
        },
    );
    let _ = event_loop.run_app(&mut app);
}
