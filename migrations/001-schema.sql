-- behe-vty baseline schema (version 1).
--
-- Fresh databases get this whole schema in one shot; every statement is
-- idempotent, so it also upgrades databases created before version tracking
-- existed. Schema changes during development go directly into this file.
-- When a release changes the schema, keep this file as-is, add
-- migrations/00N-*.sql and append it to MIGRATIONS in src/database.rs so
-- existing databases upgrade without losing data.

CREATE TABLE IF NOT EXISTS history (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    command    TEXT NOT NULL,
    device     TEXT NOT NULL DEFAULT 'local',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_history_device
    ON history (device, id);

CREATE TABLE IF NOT EXISTS devices (
    name     TEXT PRIMARY KEY,
    host     TEXT NOT NULL,
    port     INTEGER NOT NULL,
    protocol TEXT NOT NULL DEFAULT 'ssh',
    vendor   TEXT NOT NULL DEFAULT 'cisco',
    username TEXT NOT NULL DEFAULT ''
);

-- Saved logins. `password` is AES-256-GCM ciphertext (never plaintext),
-- sealed with a key derived from the user's master password and the
-- per-credential random `salt`; `nonce` is the AES-GCM nonce.
CREATE TABLE IF NOT EXISTS credentials (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    username TEXT NOT NULL,
    password BLOB NOT NULL,
    nonce    BLOB NOT NULL,
    salt     BLOB NOT NULL,
    UNIQUE (username)
);

-- Which saved login belongs to which saved device.
CREATE TABLE IF NOT EXISTS device_credentials (
    device     TEXT NOT NULL REFERENCES devices(name) ON DELETE CASCADE,
    credential INTEGER NOT NULL REFERENCES credentials(id) ON DELETE CASCADE,
    PRIMARY KEY (device, credential)
);

-- Replaced by PRAGMA user_version.
DROP TABLE IF EXISTS schema_migrations;