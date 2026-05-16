/// Migrações versionadas — aplicadas em sequência na inicialização.
/// NUNCA modificar uma migração já aplicada. Sempre adicionar novas.
pub const MIGRATIONS: &[(&str, &str)] = &[
    (
        "001_initial_schema",
        r#"
        CREATE TABLE IF NOT EXISTS clipboard_items (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            hash        TEXT    NOT NULL UNIQUE,
            content     TEXT,
            blob_path   TEXT,
            mime_type   TEXT    NOT NULL DEFAULT 'text/plain',
            byte_size   INTEGER NOT NULL DEFAULT 0,
            is_favorite INTEGER NOT NULL DEFAULT 0 CHECK(is_favorite IN (0,1)),
            is_pinned   INTEGER NOT NULL DEFAULT 0 CHECK(is_pinned   IN (0,1)),
            created_at  INTEGER NOT NULL,
            last_used   INTEGER NOT NULL,
            use_count   INTEGER NOT NULL DEFAULT 1,
            source_app  TEXT
        ) STRICT;

        CREATE INDEX IF NOT EXISTS idx_created_at
            ON clipboard_items(created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_favorites
            ON clipboard_items(is_favorite) WHERE is_favorite = 1;
        CREATE INDEX IF NOT EXISTS idx_pinned
            ON clipboard_items(is_pinned) WHERE is_pinned = 1;
        CREATE INDEX IF NOT EXISTS idx_mime_type
            ON clipboard_items(mime_type);

        -- FTS5 para busca full-text eficiente
        CREATE VIRTUAL TABLE IF NOT EXISTS items_fts USING fts5(
            content,
            content='clipboard_items',
            content_rowid='id',
            tokenize='unicode61 remove_diacritics 1'
        );

        -- Trigger: mantém FTS sincronizado com inserções
        CREATE TRIGGER IF NOT EXISTS items_ai AFTER INSERT ON clipboard_items BEGIN
            INSERT INTO items_fts(rowid, content)
            VALUES (new.id, new.content);
        END;

        -- Trigger: FTS em deleções
        CREATE TRIGGER IF NOT EXISTS items_ad AFTER DELETE ON clipboard_items BEGIN
            INSERT INTO items_fts(items_fts, rowid, content)
            VALUES ('delete', old.id, old.content);
        END;

        CREATE TABLE IF NOT EXISTS settings (
            key        TEXT    PRIMARY KEY,
            value      TEXT    NOT NULL,
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        ) STRICT;

        CREATE TABLE IF NOT EXISTS schema_migrations (
            version    TEXT    PRIMARY KEY,
            applied_at INTEGER NOT NULL DEFAULT (unixepoch())
        ) STRICT;
        "#,
    ),
];
