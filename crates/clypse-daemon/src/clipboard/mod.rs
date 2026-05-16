pub mod wayland;
pub mod x11;

use serde::{Deserialize, Serialize};

/// Evento emitido pelo monitor quando o clipboard muda.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardEvent {
    /// SHA-256 do conteúdo (hex) — usado para deduplicação
    pub hash:      String,
    /// Dados brutos do clipboard
    pub data:      Vec<u8>,
    /// MIME type negociado com o compositor/servidor X11
    pub mime_type: String,
}

/// Detecta o tipo de sessão gráfica atual.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SessionType {
    Wayland,
    X11,
}

pub fn detect_session() -> SessionType {
    let wayland_display = std::env::var("WAYLAND_DISPLAY").is_ok();
    let session_type = std::env::var("XDG_SESSION_TYPE")
        .unwrap_or_default()
        .to_lowercase();
    let is_wayland = wayland_display || session_type == "wayland";
    if is_wayland {
        SessionType::Wayland
    } else {
        SessionType::X11
    }
}

/// Inicia o monitor de clipboard na thread adequada ao ambiente.
/// Retorna um receiver de eventos.
pub fn start(
    session: SessionType,
    debounce_ms: u64,
) -> anyhow::Result<tokio::sync::mpsc::UnboundedReceiver<ClipboardEvent>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

    match session {
        SessionType::Wayland => {
            std::thread::Builder::new()
                .name("clipboard-wayland".into())
                .spawn(move || {
                    if let Err(e) = wayland::run(tx, debounce_ms) {
                        tracing::error!("Wayland clipboard monitor exited: {}", e);
                    }
                })?;
        }
        SessionType::X11 => {
            std::thread::Builder::new()
                .name("clipboard-x11".into())
                .spawn(move || {
                    if let Err(e) = x11::run(tx) {
                        tracing::error!("X11 clipboard monitor exited: {}", e);
                    }
                })?;
        }
    }

    Ok(rx)
}

#[allow(dead_code)]
/// Classifica o MIME type em categoria legível para a UI.
pub fn classify_mime(mime: &str) -> &'static str {
    if mime.starts_with("image/") {
        "image"
    } else if mime == "text/uri-list" {
        "files"
    } else if mime == "text/html" {
        "html"
    } else if mime.starts_with("text/") {
        // Heurística simples para detectar código
        "text"
    } else {
        "binary"
    }
}

/// Tenta inferir o MIME type mais específico a partir do conteúdo de texto.
pub fn refine_text_mime(content: &str) -> &'static str {
    let trimmed = content.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") || trimmed.starts_with("ftp://") {
        "text/uri"
    } else if trimmed.starts_with('{') || trimmed.starts_with('[') {
        "text/json"
    } else if trimmed.starts_with('<') && (trimmed.contains("</") || trimmed.ends_with("/>")) {
        "text/html"
    } else {
        "text/plain"
    }
}
