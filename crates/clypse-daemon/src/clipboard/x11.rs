/// Monitor de clipboard para sessões X11.
///
/// Usa x11rb com a extensão XFIXES para monitoramento event-driven:
///   XFixesSelectSelectionInput() registra interesse em mudanças da seleção CLIPBOARD.
///   O servidor X11 envia XFixesSelectionNotifyEvent quando outro cliente copia.
///
/// Este é o caminho correto — sem polling, sem subprocessos.
use super::ClipboardEvent;
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use tracing::warn;

// Fallback quando x11rb não está disponível — usa subprocess com polling inteligente
#[cfg(not(feature = "x11"))]
pub fn run(event_tx: UnboundedSender<ClipboardEvent>) -> Result<()> {
    run_subprocess_fallback(event_tx)
}

#[cfg(feature = "x11")]
pub fn run(event_tx: UnboundedSender<ClipboardEvent>) -> Result<()> {
    run_native(event_tx)
}

// ─── Implementação nativa (x11rb + XFIXES) ────────────────────────────────────

#[cfg(feature = "x11")]
fn run_native(event_tx: UnboundedSender<ClipboardEvent>) -> Result<()> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xfixes::{self, ConnectionExt as XFixesExt, SelectionEventMask};
    use x11rb::protocol::xproto::{
        self, Atom, AtomEnum, ConnectionExt, CreateWindowAux, EventMask, WindowClass,
    };
    use x11rb::rust_connection::RustConnection;

    let (conn, screen_num) = RustConnection::connect(None)?;
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;

    // Verifica se XFIXES está disponível (X11 versão >= 7.0)
    let xfixes_info = conn.xfixes_query_version(5, 0)?.reply()?;
    debug!("XFIXES version: {}.{}", xfixes_info.major_version, xfixes_info.minor_version);

    // Cria uma janela invisível para receber eventos de seleção
    let win = conn.generate_id()?;
    conn.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        win,
        root,
        -1, -1, 1, 1,
        0,
        WindowClass::INPUT_ONLY,
        x11rb::COPY_FROM_PARENT,
        &CreateWindowAux::default(),
    )?.check()?;

    // Átomos necessários
    let clipboard_atom: Atom = conn
        .intern_atom(false, b"CLIPBOARD")?.reply()?.atom;
    let utf8_atom: Atom = conn
        .intern_atom(false, b"UTF8_STRING")?.reply()?.atom;
    let targets_atom: Atom = conn
        .intern_atom(false, b"TARGETS")?.reply()?.atom;
    let clypse_atom: Atom = conn
        .intern_atom(false, b"CLYPSE_CLIPBOARD_TMP")?.reply()?.atom;

    // Registra interesse em mudanças da seleção CLIPBOARD
    conn.xfixes_select_selection_input(
        win,
        clipboard_atom,
        SelectionEventMask::SET_SELECTION_OWNER
            | SelectionEventMask::SELECTION_WINDOW_DESTROY
            | SelectionEventMask::SELECTION_CLIENT_CLOSE,
    )?.check()?;

    let xfixes_event_base = conn.extension_information(xfixes::X11_EXTENSION_NAME)?
        .map(|info| info.first_event)
        .unwrap_or(0);

    let mut last_hash = String::new();
    let debounce = Duration::from_millis(50);
    let mut last_event = Instant::now() - debounce * 2;

    conn.flush()?;

    loop {
        let event = conn.wait_for_event()?;
        let event_type = event.response_type() & !0x80;

        // XFixesSelectionNotify event
        if event_type == xfixes_event_base + xfixes::SELECTION_NOTIFY_EVENT {
            let now = Instant::now();
            if now.duration_since(last_event) < debounce {
                continue;
            }

            // Solicita o conteúdo da seleção CLIPBOARD
            conn.convert_selection(
                win,
                clipboard_atom,
                utf8_atom,
                clypse_atom,
                x11rb::CURRENT_TIME,
            )?.check()?;
            conn.flush()?;
            continue;
        }

        // SelectionNotify: o servidor respondeu ao nosso convert_selection
        if event_type == xproto::SELECTION_NOTIFY_EVENT {
            use x11rb::protocol::xproto::SelectionNotifyEvent;
            // Lê a propriedade onde o servidor colocou o conteúdo
            let prop = conn.get_property(
                true, // delete after reading
                win,
                clypse_atom,
                AtomEnum::ANY,
                0,
                u32::MAX / 4,
            )?.reply()?;

            if prop.value.is_empty() {
                continue;
            }

            let data = prop.value;
            let hash = hash_bytes(&data);

            if hash == last_hash {
                continue;
            }

            last_hash = hash.clone();
            last_event = Instant::now();

            let content_str = String::from_utf8_lossy(&data).into_owned();
            debug!(bytes = data.len(), hash = &hash[..8], "X11 clipboard captured");

            if event_tx.send(ClipboardEvent {
                hash,
                data,
                mime_type: "text/plain;charset=utf-8".into(),
            }).is_err() {
                break;
            }
        }
    }

    conn.destroy_window(win)?.check()?;
    Ok(())
}

// ─── Fallback via subprocess (sem feature x11) ───────────────────────────────

fn run_subprocess_fallback(event_tx: UnboundedSender<ClipboardEvent>) -> Result<()> {
    warn!("X11 compiled without x11rb feature — using subprocess fallback (250ms poll)");

    let mut last_hash = String::new();
    let interval = Duration::from_millis(250);

    loop {
        std::thread::sleep(interval);

        let output = std::process::Command::new("xclip")
            .args(["-selection", "clipboard", "-o"])
            .output();

        let bytes = match output {
            Ok(o) if o.status.success() && !o.stdout.is_empty() => o.stdout,
            _ => continue,
        };

        let hash = hash_bytes(&bytes);
        if hash == last_hash {
            continue;
        }
        last_hash = hash.clone();

        if event_tx.send(ClipboardEvent {
            hash,
            data: bytes,
            mime_type: "text/plain".into(),
        }).is_err() {
            break;
        }
    }

    Ok(())
}

fn hash_bytes(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}
