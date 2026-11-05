-- behe-vty schema, migration 001.
-- Applied once; recorded in the schema_migrations table by Database::open().

CREATE TABLE IF NOT EXISTS history (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    command    TEXT NOT NULL,
    device     TEXT NOT NULL DEFAULT 'local',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_history_device
    ON history (device, id);

CREATE TABLE IF NOT EXISTS devices (
    name TEXT PRIMARY KEY,
    host TEXT NOT NULL,
    port INTEGER NOT NULL
);