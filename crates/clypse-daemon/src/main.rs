mod clipboard;
mod config;
mod db;
mod ipc;

use anyhow::Result;
use clipboard::SessionType;
use config::Config;
use db::{Database, NewItem};
use ipc::IpcServer;
use ipc::protocol::Event;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    init_logging();

    info!(
        version = env!("CARGO_PKG_VERSION"),
        "Clypse daemon starting"
    );

    // ── Configuração ──────────────────────────────────────────────────────────
    let config = Config::load().unwrap_or_else(|e| {
        warn!("Failed to load config, using defaults: {}", e);
        Config::default()
    });

    // ── Banco de dados ────────────────────────────────────────────────────────
    let db_path = Config::db_path();
    let db = Database::open(&db_path)?;
    info!("Database opened at {}", db_path.display());

    // ── Servidor IPC ──────────────────────────────────────────────────────────
    let socket_path = Config::socket_path();
    let (ipc_server, event_tx) = IpcServer::new(db.clone());

    // Roda o servidor IPC em task separada
    let socket_path_clone = socket_path.clone();
    tokio::spawn(async move {
        if let Err(e) = ipc_server.run(&socket_path_clone).await {
            error!("IPC server error: {}", e);
        }
    });

    // ── Monitor de clipboard ──────────────────────────────────────────────────
    let session = clipboard::detect_session();
    info!("Session type: {:?}", session);

    let mut clip_rx = match clipboard::start(session, config.debounce_ms) {
        Ok(rx) => rx,
        Err(e) => {
            // Se Wayland falhar (compositor sem suporte), tenta X11
            if session == SessionType::Wayland {
                warn!("Wayland clipboard unavailable ({}), trying X11 fallback", e);
                clipboard::start(SessionType::X11, config.debounce_ms)?
            } else {
                return Err(e);
            }
        }
    };

    // ── Notifica systemd que estamos prontos ──────────────────────────────────
    notify_systemd_ready();

    info!("Clypse daemon ready. Monitoring clipboard...");

    // ── Loop principal: processa eventos do clipboard ─────────────────────────
    let mut watchdog = tokio::time::interval(std::time::Duration::from_secs(10));

    loop {
        tokio::select! {
            Some(event) = clip_rx.recv() => {
                let db = db.clone();
                let event_tx = event_tx.clone();
                let max_items = config.max_history_items;
                let max_image_bytes = config.max_image_size_bytes;

                tokio::task::spawn_blocking(move || {
                    process_clipboard_event(event, db, event_tx, max_items, max_image_bytes);
                });
            }

            _ = watchdog.tick() => {
                notify_systemd("WATCHDOG=1");
            }

            // Sinais de shutdown
            _ = shutdown_signal() => {
                info!("Shutdown signal received");
                break;
            }
        }
    }

    // Cleanup
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }

    info!("Clypse daemon stopped");
    Ok(())
}

// ─── Processamento de evento ──────────────────────────────────────────────────

fn process_clipboard_event(
    event: clipboard::ClipboardEvent,
    db: Database,
    event_tx: tokio::sync::broadcast::Sender<Event>,
    max_items: usize,
    max_image_bytes: u64,
) {
    // Valida tamanho máximo para imagens
    if event.mime_type.starts_with("image/") && event.data.len() as u64 > max_image_bytes {
        warn!(
            bytes = event.data.len(),
            "Image too large, skipping"
        );
        return;
    }

    // Resolve MIME type e conteúdo textual
    let (content, mime_type) = if is_text_mime(&event.mime_type) {
        match String::from_utf8(event.data.clone()) {
            Ok(s) if !s.trim().is_empty() => {
                let refined = clipboard::refine_text_mime(&s);
                (Some(s), refined.to_string())
            }
            Ok(_) => return, // string vazia
            Err(_) => {
                warn!("Non-UTF8 text clipboard, skipping");
                return;
            }
        }
    } else {
        (None, event.mime_type.clone())
    };

    let blob_path = if content.is_none() {
        save_image_blob(&event.hash, &event.data, &event.mime_type)
    } else {
        None
    };

    let new_item = NewItem {
        hash:      &event.hash,
        content:   content.as_deref(),
        blob_path: blob_path.as_deref(),
        mime_type: &mime_type,
        byte_size: event.data.len(),
    };

    match db.upsert_item(new_item) {
        Ok((id, is_new)) => {
            // Aplica limite de histórico
            if let Err(e) = db.enforce_limit(max_items) {
                warn!("Failed to enforce history limit: {}", e);
            }

            // Toma ownership do clipboard no Wayland para preservar conteúdo
            // (apenas para sessões Wayland)
            if clipboard::detect_session() == SessionType::Wayland {
                clipboard::wayland::take_ownership(event.data, &event.mime_type);
            }

            // Notifica clientes GUI via broadcast
            if is_new {
                if let Ok(Some(item)) = db.get_item(id) {
                    let _ = event_tx.send(Event::ItemAdded { item });
                }
            } else {
                let _ = event_tx.send(Event::ItemUpdated { item_id: id });
            }

            tracing::debug!(
                id = id,
                is_new = is_new,
                mime = %mime_type,
                "Clipboard item processed"
            );
        }
        Err(e) => {
            error!("Failed to store clipboard item: {}", e);
        }
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Grava os bytes de uma imagem em `~/.local/share/clypse/images/` e retorna o caminho.
fn save_image_blob(hash: &str, data: &[u8], mime_type: &str) -> Option<String> {
    let images_dir = Config::images_dir();
    std::fs::create_dir_all(&images_dir).ok()?;
    // "image/svg+xml" → "svg"
    let ext = mime_type.split('/').nth(1).unwrap_or("bin")
        .split('+').next().unwrap_or("bin");
    let filename = format!("{}.{}", &hash[..16], ext);
    let path = images_dir.join(&filename);
    if !path.exists() {
        std::fs::write(&path, data).ok()?;
    }
    path.to_str().map(str::to_string)
}

fn is_text_mime(mime: &str) -> bool {
    mime.starts_with("text/") || mime == "application/json" || mime == "application/xml"
}

fn init_logging() {
    use tracing_subscriber::{fmt, EnvFilter};

    // CLYPSE_LOG=debug para habilitar logs detalhados
    let filter = EnvFilter::try_from_env("CLYPSE_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info"));

    fmt::Subscriber::builder()
        .with_env_filter(filter)
        .with_target(false)
        .with_thread_names(true)
        .init();
}

fn notify_systemd_ready() {
    notify_systemd("READY=1");
}

fn notify_systemd(msg: &str) {
    if let Ok(sock_path) = std::env::var("NOTIFY_SOCKET") {
        use std::os::unix::net::UnixDatagram;
        if let Ok(sock) = UnixDatagram::unbound() {
            let _ = sock.send_to(msg.as_bytes(), &sock_path);
        }
    }
}

async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM handler");
    let mut sigint  = signal(SignalKind::interrupt()).expect("SIGINT handler");

    tokio::select! {
        _ = sigterm.recv() => {},
        _ = sigint.recv()  => {},
    }
}
