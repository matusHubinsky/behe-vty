use rusqlite::{params, Connection, Result};
use std::path::PathBuf;

pub struct Database {
    conn: Connection,
}

#[derive(Debug, Clone)]
pub struct Device {
    pub name: String,
    pub host: String,
    pub port: u16,
}

fn db_path() -> PathBuf {
    let mut dir = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    dir.push("behe-vty");
    let _ = std::fs::create_dir_all(&dir);
    dir.push("data.db");
    dir
}

impl Database {
    pub fn open() -> Result<Self> {
        let conn = Connection::open(db_path())?;

        conn.execute_batch(
            "
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
            ",
        )?;

        Ok(Self { conn })
    }

    // ---------------- history

    pub fn add_history(&self, device: &str, command: &str) -> Result<()> {
        if command.trim().is_empty() {
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO history (command, device) VALUES (?1, ?2)",
            params![command, device],
        )?;
        Ok(())
    }

    /// The last `limit` commands for a device, oldest first.
    pub fn load_history(&self, device: &str, limit: usize) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT command FROM (
                 SELECT id, command FROM history
                 WHERE device = ?1
                 ORDER BY id DESC
                 LIMIT ?2
             ) ORDER BY id ASC",
        )?;

        let rows = stmt.query_map(params![device, limit as i64], |row| row.get(0))?;
        rows.collect()
    }

    // ---------------- devices

    pub fn save_device(&self, name: &str, host: &str, port: u16) -> Result<()> {
        self.conn.execute(
            "INSERT INTO devices (name, host, port) VALUES (?1, ?2, ?3)
             ON CONFLICT(name) DO UPDATE SET host = ?2, port = ?3",
            params![name, host, port],
        )?;
        Ok(())
    }

    pub fn get_device(&self, name: &str) -> Result<Option<Device>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, host, port FROM devices WHERE name = ?1")?;

        let mut rows = stmt.query_map(params![name], |row| {
            Ok(Device {
                name: row.get(0)?,
                host: row.get(1)?,
                port: row.get(2)?,
            })
        })?;

        rows.next().transpose()
    }

    pub fn list_devices(&self) -> Result<Vec<Device>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, host, port FROM devices ORDER BY name")?;

        let rows = stmt.query_map([], |row| {
            Ok(Device {
                name: row.get(0)?,
                host: row.get(1)?,
                port: row.get(2)?,
            })
        })?;

        rows.collect()
    }

    pub fn delete_device(&self, name: &str) -> Result<bool> {
        let n = self
            .conn
            .execute("DELETE FROM devices WHERE name = ?1", params![name])?;
        Ok(n > 0)
    }
}
