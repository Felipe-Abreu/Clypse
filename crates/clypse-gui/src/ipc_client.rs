/// Cliente IPC — conecta ao daemon via Unix socket.
///
/// Roda em thread separada (tokio single-thread runtime).
/// Comunica com a GUI via std::sync::mpsc — thread-safe e sem dependência de GLib.
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::mpsc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tracing::{debug, warn};

// ─── Tipos públicos ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipItem {
    pub id:          i64,
    pub hash:        String,
    pub content:     Option<String>,
    pub blob_path:   Option<String>,
    pub mime_type:   String,
    pub byte_size:   i64,
    pub is_favorite: bool,
    pub is_pinned:   bool,
    pub created_at:  i64,
    pub last_used:   i64,
    pub use_count:   i64,
}

/// Mensagens do daemon para a GUI
#[derive(Debug, Clone)]
pub enum DaemonMessage {
    Items           { items: Vec<ClipItem>, total: i64 },
    ItemAdded       (ClipItem),
    FavoriteToggled { item_id: i64, is_favorite: bool },
    PinnedToggled   { item_id: i64, is_pinned: bool },
    ItemUpdated,
    ItemDeleted     { item_id: i64 },
    HistoryCleared,
    Disconnected,
    Error           (String),
}

/// Comandos da GUI para o daemon via IPC
#[derive(Debug, Clone)]
pub enum GuiCommand {
    GetItems      { search: Option<String>, limit: usize, offset: usize },
    ToggleFavorite(i64),
    DeleteItem    (i64),
    ClearHistory,
}

// ─── Inicialização ────────────────────────────────────────────────────────────

/// Inicia o cliente IPC em thread dedicada.
/// Retorna (sender de comandos, receiver de mensagens do daemon).
/// O receiver é `!Send` — deve permanecer na thread GTK.
pub fn start(
    socket_path: PathBuf,
) -> (mpsc::SyncSender<GuiCommand>, mpsc::Receiver<DaemonMessage>) {
    let (cmd_tx, cmd_rx) = mpsc::sync_channel::<GuiCommand>(32);
    let (msg_tx, msg_rx) = mpsc::sync_channel::<DaemonMessage>(256);

    std::thread::Builder::new()
        .name("ipc-client".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime for IPC client");

            rt.block_on(ipc_loop(socket_path, cmd_rx, msg_tx));
        })
        .expect("IPC client thread failed to start");

    (cmd_tx, msg_rx)
}

// ─── Loop do cliente IPC ─────────────────────────────────────────────────────

async fn ipc_loop(
    socket_path: PathBuf,
    cmd_rx: mpsc::Receiver<GuiCommand>,
    msg_tx: mpsc::SyncSender<DaemonMessage>,
) {
    loop {
        match connect_and_run(&socket_path, &cmd_rx, &msg_tx).await {
            Ok(()) => break, // saída limpa — IPC encerrou normalmente
            Err(e) => {
                warn!("IPC connection lost: {}. Retrying in 2s...", e);
                let _ = msg_tx.send(DaemonMessage::Disconnected);
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            }
        }
    }
}

async fn connect_and_run(
    socket_path: &PathBuf,
    cmd_rx: &mpsc::Receiver<GuiCommand>,
    msg_tx: &mpsc::SyncSender<DaemonMessage>,
) -> Result<()> {
    let stream = UnixStream::connect(socket_path)
        .await
        .map_err(|e| anyhow!("Cannot connect to {}: {}", socket_path.display(), e))?;

    debug!("IPC connected to daemon at {}", socket_path.display());

    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let mut request_id: u64 = 1;

    // Solicita lista inicial imediatamente após conectar
    writer.write_all(get_items_request(request_id, None, 100, 0).as_bytes()).await?;
    request_id += 1;

    loop {
        // Drena comandos pendentes da GUI (não-bloqueante)
        loop {
            match cmd_rx.try_recv() {
                Ok(cmd) => {
                    let req = command_to_json(&cmd, request_id);
                    request_id += 1;
                    writer.write_all(req.as_bytes()).await?;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
            }
        }

        // Aguarda resposta do daemon (com timeout para checar comandos periodicamente)
        tokio::select! {
            line = lines.next_line() => {
                match line? {
                    None => return Err(anyhow!("daemon closed connection")),
                    Some(raw) => {
                        debug!("IPC ← {:.120}", raw);
                        if let Some(msg) = parse_daemon_message(&raw) {
                            // Ignora erro de send se a GUI encerrou
                            let _ = msg_tx.send(msg);
                        }
                    }
                }
            }
            _ = tokio::time::sleep(tokio::time::Duration::from_millis(20)) => {
                // Tick: verifica comandos novamente
            }
        }
    }
}

// ─── Serialização de requests ─────────────────────────────────────────────────

fn get_items_request(id: u64, search: Option<&str>, limit: usize, offset: usize) -> String {
    let search_json = match search {
        Some(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        None    => "null".into(),
    };
    format!(
        "{{\"method\":\"get_items\",\"id\":{},\"search\":{},\"limit\":{},\"offset\":{}}}\n",
        id, search_json, limit, offset
    )
}

fn command_to_json(cmd: &GuiCommand, id: u64) -> String {
    match cmd {
        GuiCommand::GetItems { search, limit, offset } => {
            get_items_request(id, search.as_deref(), *limit, *offset)
        }
        GuiCommand::ToggleFavorite(item_id) => {
            format!("{{\"method\":\"toggle_favorite\",\"id\":{},\"item_id\":{}}}\n", id, item_id)
        }
        GuiCommand::DeleteItem(item_id) => {
            format!("{{\"method\":\"delete_item\",\"id\":{},\"item_id\":{}}}\n", id, item_id)
        }
        GuiCommand::ClearHistory => {
            format!("{{\"method\":\"clear_history\",\"id\":{}}}\n", id)
        }
    }
}

// ─── Parsing de respostas ─────────────────────────────────────────────────────

fn parse_daemon_message(raw: &str) -> Option<DaemonMessage> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let msg_type = v["type"].as_str()?;

    match msg_type {
        "result" => {
            let data = &v["data"];
            match data["kind"].as_str()? {
                "items" => {
                    let items: Vec<ClipItem> = serde_json::from_value(data["items"].clone()).ok()?;
                    let total = data["total"].as_i64().unwrap_or(0);
                    Some(DaemonMessage::Items { items, total })
                }
                "deleted"  => Some(DaemonMessage::ItemDeleted { item_id: data["item_id"].as_i64()? }),
                "cleared"  => Some(DaemonMessage::HistoryCleared),
                "favorite" => Some(DaemonMessage::FavoriteToggled {
                    item_id:     data["item_id"].as_i64()?,
                    is_favorite: data["is_favorite"].as_bool()?,
                }),
                "pinned"   => Some(DaemonMessage::PinnedToggled {
                    item_id:  data["item_id"].as_i64()?,
                    is_pinned: data["is_pinned"].as_bool()?,
                }),
                _          => None,
            }
        }
        "event" => {
            let ev = &v["event"];
            match ev["kind"].as_str()? {
                "item_added"      => {
                    let item: ClipItem = serde_json::from_value(ev["item"].clone()).ok()?;
                    Some(DaemonMessage::ItemAdded(item))
                }
                "item_updated"    => Some(DaemonMessage::ItemUpdated),
                "item_deleted"    => Some(DaemonMessage::ItemDeleted { item_id: ev["item_id"].as_i64()? }),
                "history_cleared" => Some(DaemonMessage::HistoryCleared),
                _                 => None,
            }
        }
        "error" => Some(DaemonMessage::Error(
            v["message"].as_str().unwrap_or("unknown").to_string()
        )),
        _ => None,
    }
}
