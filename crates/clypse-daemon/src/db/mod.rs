mod migrations;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

pub use migrations::MIGRATIONS;

// ─── Tipos públicos ────────────────────────────────────────────────────────────

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

#[derive(Debug)]
pub struct NewItem<'a> {
    pub hash:      &'a str,
    pub content:   Option<&'a str>,
    pub blob_path: Option<&'a str>,
    pub mime_type: &'a str,
    pub byte_size: usize,
}

// ─── Handle thread-safe ───────────────────────────────────────────────────────

#[derive(Clone)]
pub struct Database(Arc<Mutex<Connection>>);

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        std::fs::create_dir_all(path.parent().unwrap())?;

        let conn = Connection::open(path)
            .with_context(|| format!("opening database at {}", path.display()))?;

        // Configuração de performance e confiabilidade
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous  = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA cache_size   = -16000;
             PRAGMA temp_store   = MEMORY;
             PRAGMA mmap_size    = 134217728;
             PRAGMA busy_timeout = 5000;",
        )?;

        let db = Self(Arc::new(Mutex::new(conn)));
        db.run_migrations()?;
        Ok(db)
    }

    fn run_migrations(&self) -> Result<()> {
        let conn = self.0.lock().unwrap();

        // Garante que a tabela de controle existe antes da primeira migração
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                 version    TEXT    PRIMARY KEY,
                 applied_at INTEGER NOT NULL DEFAULT (unixepoch())
             ) STRICT;",
        )?;

        for (version, sql) in MIGRATIONS {
            let applied: bool = conn
                .query_row(
                    "SELECT 1 FROM schema_migrations WHERE version = ?1",
                    params![version],
                    |_| Ok(true),
                )
                .optional()?
                .unwrap_or(false);

            if !applied {
                conn.execute_batch(sql)?;
                conn.execute(
                    "INSERT INTO schema_migrations (version) VALUES (?1)",
                    params![version],
                )?;
                tracing::info!("Applied migration: {}", version);
            }
        }
        Ok(())
    }

    // ─── Escrita ───────────────────────────────────────────────────────────────

    /// Insere um novo item ou atualiza last_used/use_count se o hash já existe.
    /// Retorna (item_id, is_new).
    pub fn upsert_item(&self, item: NewItem<'_>) -> Result<(i64, bool)> {
        let now = now_ms();
        let conn = self.0.lock().unwrap();

        // Tenta atualizar se já existe (deduplicação por hash)
        let updated = conn.execute(
            "UPDATE clipboard_items
             SET last_used = ?1, use_count = use_count + 1
             WHERE hash = ?2",
            params![now, item.hash],
        )?;

        if updated > 0 {
            let id: i64 = conn.query_row(
                "SELECT id FROM clipboard_items WHERE hash = ?1",
                params![item.hash],
                |r| r.get(0),
            )?;
            return Ok((id, false));
        }

        // Item novo
        conn.execute(
            "INSERT INTO clipboard_items
             (hash, content, blob_path, mime_type, byte_size, created_at, last_used)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![
                item.hash,
                item.content,
                item.blob_path,
                item.mime_type,
                item.byte_size as i64,
                now,
            ],
        )?;

        let id = conn.last_insert_rowid();
        Ok((id, true))
    }

    /// Aplica o limite de itens mantendo favoritos e pinados.
    pub fn enforce_limit(&self, max_items: usize) -> Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "DELETE FROM clipboard_items
             WHERE is_favorite = 0 AND is_pinned = 0
               AND id NOT IN (
                   SELECT id FROM clipboard_items
                   WHERE is_favorite = 0 AND is_pinned = 0
                   ORDER BY last_used DESC
                   LIMIT ?1
               )",
            params![max_items as i64],
        )?;
        Ok(())
    }

    pub fn delete_item(&self, id: i64) -> Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute("DELETE FROM clipboard_items WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn toggle_favorite(&self, id: i64) -> Result<bool> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "UPDATE clipboard_items
             SET is_favorite = ((is_favorite | 1) - (is_favorite & 1))
             WHERE id = ?1",
            params![id],
        )?;
        let new_val: i64 = conn.query_row(
            "SELECT is_favorite FROM clipboard_items WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        Ok(new_val == 1)
    }

    pub fn toggle_pinned(&self, id: i64) -> Result<bool> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "UPDATE clipboard_items
             SET is_pinned = ((is_pinned | 1) - (is_pinned & 1))
             WHERE id = ?1",
            params![id],
        )?;
        let new_val: i64 = conn.query_row(
            "SELECT is_pinned FROM clipboard_items WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        Ok(new_val == 1)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, unixepoch())
             ON CONFLICT(key) DO UPDATE SET value = excluded.value,
                                            updated_at = excluded.updated_at",
            params![key, value],
        )?;
        Ok(())
    }

    // ─── Leitura ───────────────────────────────────────────────────────────────

    pub fn get_items(&self, search: Option<&str>, limit: usize, offset: usize) -> Result<Vec<ClipItem>> {
        let conn = self.0.lock().unwrap();

        if let Some(query) = search.filter(|s| !s.trim().is_empty()) {
            // FTS5: busca full-text com rank por relevância
            let mut stmt = conn.prepare(
                "SELECT c.id, c.hash, c.content, c.blob_path, c.mime_type,
                        c.byte_size, c.is_favorite, c.is_pinned,
                        c.created_at, c.last_used, c.use_count
                 FROM items_fts
                 JOIN clipboard_items c ON items_fts.rowid = c.id
                 WHERE items_fts MATCH ?1
                 ORDER BY rank
                 LIMIT ?2 OFFSET ?3",
            )?;
            collect_items(&mut stmt, params![format!("\"{}\"*", query.replace('"', "")), limit as i64, offset as i64])
        } else {
            let mut stmt = conn.prepare(
                "SELECT id, hash, content, blob_path, mime_type,
                        byte_size, is_favorite, is_pinned,
                        created_at, last_used, use_count
                 FROM clipboard_items
                 ORDER BY is_pinned DESC, last_used DESC
                 LIMIT ?1 OFFSET ?2",
            )?;
            collect_items(&mut stmt, params![limit as i64, offset as i64])
        }
    }

    pub fn get_item(&self, id: i64) -> Result<Option<ClipItem>> {
        let conn = self.0.lock().unwrap();
        conn.query_row(
            "SELECT id, hash, content, blob_path, mime_type,
                    byte_size, is_favorite, is_pinned,
                    created_at, last_used, use_count
             FROM clipboard_items WHERE id = ?1",
            params![id],
            row_to_item,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Item usado mais recentemente — para restaurar o clipboard após restart.
    pub fn latest_item(&self) -> Result<Option<ClipItem>> {
        let conn = self.0.lock().unwrap();
        conn.query_row(
            "SELECT id, hash, content, blob_path, mime_type,
                    byte_size, is_favorite, is_pinned,
                    created_at, last_used, use_count
             FROM clipboard_items
             ORDER BY last_used DESC
             LIMIT 1",
            [],
            row_to_item,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn get_setting(&self, key: &str, default: &str) -> Result<String> {
        let conn = self.0.lock().unwrap();
        conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![key],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map(|v| v.unwrap_or_else(|| default.to_string()))
        .map_err(Into::into)
    }

    pub fn count(&self) -> Result<i64> {
        let conn = self.0.lock().unwrap();
        Ok(conn.query_row("SELECT COUNT(*) FROM clipboard_items", [], |r| r.get(0))?)
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn row_to_item(r: &rusqlite::Row<'_>) -> rusqlite::Result<ClipItem> {
    Ok(ClipItem {
        id:          r.get(0)?,
        hash:        r.get(1)?,
        content:     r.get(2)?,
        blob_path:   r.get(3)?,
        mime_type:   r.get(4)?,
        byte_size:   r.get(5)?,
        is_favorite: r.get::<_, i64>(6)? == 1,
        is_pinned:   r.get::<_, i64>(7)? == 1,
        created_at:  r.get(8)?,
        last_used:   r.get(9)?,
        use_count:   r.get(10)?,
    })
}

fn collect_items(
    stmt: &mut rusqlite::Statement<'_>,
    params: impl rusqlite::Params,
) -> Result<Vec<ClipItem>> {
    let rows = stmt.query_map(params, row_to_item)?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?);
    }
    Ok(items)
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
