mod ipc_client;
mod settings;
mod tray;
mod window;

use anyhow::Result;
use gtk4::glib;
use gtk4::gio;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use std::path::PathBuf;
use tracing::{error, info, warn};
use window::ClypseWindow;

const APP_ID: &str = "io.github.clypse.Clypse";

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("CLYPSE_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    // Mantém o processo vivo quando a janela é fechada (tray continua ativo)
    app.connect_startup(|app| {
        let _ = app.hold();
    });

    // Primeira invocação: abre a janela. Segunda invocação (atalho): apresenta ou mostra.
    app.connect_command_line(|app, _| {
        if app.windows().is_empty() {
            app.activate();
        } else {
            for window in app.windows() {
                window.present();
            }
        }
        0
    });

    app.connect_activate(|app| {
        if let Err(e) = build_ui(app) {
            error!("Failed to build UI: {}", e);
        }
    });

    // CSS global mínimo
    app.connect_startup(|_| {
        let provider = gtk4::CssProvider::new();
        provider.load_from_string(
            "listbox row { border-radius: 8px; }\n\
             listbox actionrow { border-radius: 8px; }\n"
        );
        if let Some(display) = gtk4::gdk::Display::default() {
            gtk4::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
    });

    // Ações globais da aplicação
    let quit = gio::SimpleAction::new("quit", None);
    quit.connect_activate({
        let app_ref = app.downgrade();
        move |_, _| {
            if let Some(app) = app_ref.upgrade() {
                app.quit();
            }
        }
    });
    app.add_action(&quit);
    app.set_accels_for_action("app.quit", &["<Ctrl>Q"]);

    let about = gio::SimpleAction::new("about", None);
    about.connect_activate(|_, _| show_about());
    app.add_action(&about);

    let preferences = gio::SimpleAction::new("preferences", None);
    preferences.connect_activate(|_, _| {
        let win = gtk4::Application::default().active_window();
        settings::show(win.as_ref());
    });
    app.add_action(&preferences);
    app.set_accels_for_action("app.preferences", &["<Ctrl>comma"]);

    let code = app.run();
    std::process::exit(code.value());
}

fn build_ui(app: &adw::Application) -> Result<()> {
    let socket_path = daemon_socket_path();

    if !socket_path.exists() {
        if let Err(e) = start_daemon() {
            warn!("Could not start daemon: {}", e);
        }
        let mut waited = 0u32;
        while !socket_path.exists() && waited < 30 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            waited += 1;
        }
    }

    // Inicia cliente IPC em thread separada
    let (cmd_tx, msg_rx) = ipc_client::start(socket_path);

    // Janela principal
    let clypse_win = ClypseWindow::new(app, cmd_tx);

    // Fecha a janela (hide) sem destruir — o app fica vivo no tray
    {
        let win = clypse_win.window.clone();
        win.connect_close_request(move |w| {
            w.set_visible(false);
            glib::Propagation::Stop
        });
    }

    // Inicia ícone SNI no painel (thread separada)
    let tray_rx = tray::start();

    // Drena receiver IPC + ações do tray no loop GTK via timeout
    {
        let win       = clypse_win.clone();
        let app_weak  = app.downgrade();
        glib::timeout_add_local(std::time::Duration::from_millis(20), move || {
            // Mensagens do daemon
            while let Ok(msg) = msg_rx.try_recv() {
                win.handle_daemon_message(msg);
            }
            // Ações do tray
            while let Ok(action) = tray_rx.try_recv() {
                match action {
                    tray::TrayAction::Show => win.show_at_cursor(),
                    tray::TrayAction::Quit => {
                        if let Some(app) = app_weak.upgrade() {
                            app.quit();
                        }
                    }
                }
            }
            glib::ControlFlow::Continue
        });
    }

    clypse_win.window.present();
    info!("Clypse GUI started");
    Ok(())
}

fn show_about() {
    let dialog = adw::AboutDialog::builder()
        .application_name("Clypse")
        .application_icon(APP_ID)
        .developer_name("Felipe Abreu")
        .version(env!("CARGO_PKG_VERSION"))
        .website("https://github.com/Felipe-Abreu/Clypse")
        .issue_url("https://github.com/Felipe-Abreu/Clypse/issues")
        .license_type(gtk4::License::Gpl30)
        .comments(
            "A modern, fast clipboard manager for Linux.\n\
             Built with Rust, GTK4, and libadwaita."
        )
        .build();

    let active = gtk4::Application::default().active_window();
    dialog.present(active.as_ref());
}

// ─── Daemon management ────────────────────────────────────────────────────────

fn daemon_socket_path() -> PathBuf {
    std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
        .join("clypse/daemon.sock")
}

fn start_daemon() -> Result<()> {
    // Tenta via systemd primeiro
    if std::process::Command::new("systemctl")
        .args(["--user", "start", "clypse-daemon.service"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        info!("Daemon started via systemd");
        return Ok(());
    }

    // Fallback: inicia diretamente
    let daemon_bin = which::which("clypse-daemon")
        .map_err(|_| anyhow::anyhow!("clypse-daemon not found. Is it installed?"))?;

    std::process::Command::new(&daemon_bin)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    info!("Daemon started: {}", daemon_bin.display());
    Ok(())
}
