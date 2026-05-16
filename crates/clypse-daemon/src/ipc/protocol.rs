use crate::db::ClipItem;
use serde::{Deserialize, Serialize};

// ─── Requests (GUI → Daemon) ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Request {
    /// Lista itens com paginação e busca opcional
    GetItems {
        id:     u64,
        search: Option<String>,
        limit:  Option<usize>,
        offset: Option<usize>,
    },
    /// Busca item individual por id
    GetItem { id: u64, item_id: i64 },
    /// Alterna favorito; retorna novo estado
    ToggleFavorite { id: u64, item_id: i64 },
    /// Alterna pin; retorna novo estado
    TogglePinned { id: u64, item_id: i64 },
    /// Remove item
    DeleteItem { id: u64, item_id: i64 },
    /// Remove todos os itens não-favoritos e não-pinados
    ClearHistory { id: u64 },
    /// Lê configuração
    GetSetting { id: u64, key: String },
    /// Grava configuração
    SetSetting { id: u64, key: String, value: String },
    /// Status do daemon
    Status { id: u64 },
}

impl Request {
    pub fn request_id(&self) -> u64 {
        match self {
            Self::GetItems    { id, .. } => *id,
            Self::GetItem     { id, .. } => *id,
            Self::ToggleFavorite { id, .. } => *id,
            Self::TogglePinned   { id, .. } => *id,
            Self::DeleteItem  { id, .. } => *id,
            Self::ClearHistory{ id, .. } => *id,
            Self::GetSetting  { id, .. } => *id,
            Self::SetSetting  { id, .. } => *id,
            Self::Status      { id, .. } => *id,
        }
    }
}

// ─── Responses (Daemon → GUI) ─────────────────────────────────────────────────

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    /// Resposta a um Request
    Result {
        id:   u64,
        data: ResponseData,
    },
    /// Erro a um Request
    Error {
        id:      u64,
        message: String,
    },
    /// Notificação push — sem id (daemon → GUI)
    Event { event: Event },
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(dead_code)]
pub enum ResponseData {
    Items      { items: Vec<ClipItem>, total: i64 },
    Item       { item: Option<ClipItem> },
    Favorite   { item_id: i64, is_favorite: bool },
    Pinned     { item_id: i64, is_pinned: bool },
    Deleted    { item_id: i64 },
    Cleared    { removed: usize },
    Setting    { key: String, value: String },
    SettingSet { key: String },
    Status     { version: &'static str, item_count: i64, uptime_secs: u64 },
    Ok,
}

// ─── Events (push do daemon para todos os clientes conectados) ────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(dead_code)]
pub enum Event {
    /// Novo item capturado do clipboard
    ItemAdded   { item: ClipItem },
    /// Item atualizado (dedup — use_count incrementado)
    ItemUpdated { item_id: i64 },
    /// Item removido
    ItemDeleted { item_id: i64 },
    /// Histórico limpo
    HistoryCleared,
    #[allow(dead_code)]
    Ok,
}

// Suprime dead_code para variantes que serão usadas por clientes IPC futuros
#[allow(dead_code)]
const _: () = ();
