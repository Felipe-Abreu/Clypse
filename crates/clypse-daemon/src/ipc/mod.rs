pub mod protocol;

use crate::db::Database;
use anyhow::Result;
use protocol::{Event, Request, Response, ResponseData};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

// ─── Servidor IPC ─────────────────────────────────────────────────────────────

pub struct IpcServer {
    event_tx: broadcast::Sender<Event>,
    db:       Database,
    started:  Instant,
}

impl IpcServer {
    pub fn new(db: Database) -> (Self, broadcast::Sender<Event>) {
        let (event_tx, _) = broadcast::channel(256);
        let srv = Self {
            event_tx: event_tx.clone(),
            db,
            started: Instant::now(),
        };
        (srv, event_tx)
    }

    pub async fn run(self, socket_path: &Path) -> Result<()> {
        // Remove socket stale de execução anterior
        if socket_path.exists() {
            std::fs::remove_file(socket_path)?;
        }
        std::fs::create_dir_all(socket_path.parent().unwrap())?;

        let listener = UnixListener::bind(socket_path)?;
        info!("IPC server listening at {}", socket_path.display());

        let srv = Arc::new(self);

        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    let handler = Arc::clone(&srv);
                    tokio::spawn(async move {
                        if let Err(e) = handler.handle_client(stream).await {
                            warn!("IPC client error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    error!("IPC accept error: {}", e);
                }
            }
        }
    }

    /// Envia um evento para todos os clientes conectados.
    #[allow(dead_code)]
    pub fn broadcast(&self, event: Event) {
        // Ignora erro se não há clientes conectados
        let _ = self.event_tx.send(event);
    }

    async fn handle_client(&self, stream: UnixStream) -> Result<()> {
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();
        let mut event_rx = self.event_tx.subscribe();

        loop {
            tokio::select! {
                // Linha recebida do cliente (request)
                line = lines.next_line() => {
                    match line? {
                        None => break, // cliente fechou a conexão
                        Some(raw) => {
                            debug!("IPC ← {}", &raw[..raw.len().min(120)]);
                            let response = self.dispatch(&raw).await;
                            let mut serialized = serde_json::to_string(&response)?;
                            serialized.push('\n');
                            writer.write_all(serialized.as_bytes()).await?;
                        }
                    }
                }
                // Evento de broadcast para este cliente
                evt = event_rx.recv() => {
                    match evt {
                        Ok(event) => {
                            let msg = Response::Event { event };
                            let mut serialized = serde_json::to_string(&msg)?;
                            serialized.push('\n');
                            if writer.write_all(serialized.as_bytes()).await.is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!("IPC client lagged, dropped {} events", n);
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
        Ok(())
    }

    async fn dispatch(&self, raw: &str) -> Response {
        let req: Request = match serde_json::from_str(raw) {
            Ok(r) => r,
            Err(e) => {
                return Response::Error { id: 0, message: format!("parse error: {}", e) };
            }
        };

        let req_id = req.request_id();

        // Executa operações de DB no thread blocking para não bloquear o runtime
        let db = self.db.clone();
        let started = self.started;

        let result = tokio::task::spawn_blocking(move || {
            Self::execute_request(req, db, started)
        })
        .await;

        match result {
            Ok(Ok(data)) => Response::Result { id: req_id, data },
            Ok(Err(e))   => Response::Error  { id: req_id, message: e.to_string() },
            Err(e)       => Response::Error  { id: req_id, message: format!("internal: {}", e) },
        }
    }

    fn execute_request(req: Request, db: Database, started: Instant) -> Result<ResponseData> {
        match req {
            Request::GetItems { search, limit, offset, .. } => {
                let items = db.get_items(
                    search.as_deref(),
                    limit.unwrap_or(50).min(200),
                    offset.unwrap_or(0),
                )?;
                let total = db.count()?;
                Ok(ResponseData::Items { items, total })
            }

            Request::GetItem { item_id, .. } => {
                let item = db.get_item(item_id)?;
                Ok(ResponseData::Item { item })
            }

            Request::ToggleFavorite { item_id, .. } => {
                let is_favorite = db.toggle_favorite(item_id)?;
                Ok(ResponseData::Favorite { item_id, is_favorite })
            }

            Request::TogglePinned { item_id, .. } => {
                let is_pinned = db.toggle_pinned(item_id)?;
                Ok(ResponseData::Pinned { item_id, is_pinned })
            }

            Request::DeleteItem { item_id, .. } => {
                db.delete_item(item_id)?;
                Ok(ResponseData::Deleted { item_id })
            }

            Request::ClearHistory { .. } => {
                let before = db.count()?;
                db.enforce_limit(0)?;
                let after = db.count()?;
                Ok(ResponseData::Cleared { removed: (before - after) as usize })
            }

            Request::GetSetting { key, .. } => {
                let value = db.get_setting(&key, "")?;
                Ok(ResponseData::Setting { key, value })
            }

            Request::SetSetting { key, value, .. } => {
                db.set_setting(&key, &value)?;
                Ok(ResponseData::SettingSet { key })
            }

            Request::Status { .. } => {
                let item_count = db.count()?;
                Ok(ResponseData::Status {
                    version:     env!("CARGO_PKG_VERSION"),
                    item_count,
                    uptime_secs: started.elapsed().as_secs(),
                })
            }
        }
    }
}
