// Copyright (c) 2026 - Matus Hubinsky

// Licensed under the EUPL-1.2-only
// You may not use this work except in compliance with the Licence.
// You may obtain a copy of the Licence at:

// https://joinup.ec.europa.eu/software/page/eupl

// Unless required by applicable law or agreed to in writing, software
// distributed under the Licence is distributed on an "AS IS" basis,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the Licence for the specific language governing permissions and
// limitations under the Licence.

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
    // Test/override hook: BEHE_VTY_DB=/tmp/x.db behe-vty
    if let Some(p) = std::env::var_os("BEHE_VTY_DB") {
        return PathBuf::from(p);
    }
    let mut dir = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    dir.push("behe-vty");
    let _ = std::fs::create_dir_all(&dir);
    dir.push("data.db");
    dir
}

/// Schema migrations, embedded at compile time. Each entry is applied at most
/// once and recorded in the `schema_migrations` table.
const MIGRATIONS: &[(&str, &str)] = &[("001-init", include_str!("../migrations/001-init.sql"))];

impl Database {
    pub fn open() -> Result<Self> {
        let mut conn = Connection::open(db_path())?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                 version    TEXT PRIMARY KEY,
                 applied_at TEXT NOT NULL DEFAULT (datetime('now'))
             );",
        )?;
        for (version, sql) in MIGRATIONS {
            let applied: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = ?1)",
                params![version],
                |r| r.get(0),
            )?;
            if !applied {
                let tx = conn.transaction()?;
                tx.execute_batch(sql)?;
                tx.execute(
                    "INSERT INTO schema_migrations (version) VALUES (?1)",
                    params![version],
                )?;
                tx.commit()?;
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Database {
        let path = std::env::temp_dir().join(format!("behe-vty-test-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::env::set_var("BEHE_VTY_DB", &path);
        Database::open().unwrap()
    }

    #[test]
    fn test_history_roundtrip_and_limit() {
        let db = test_db();
        db.add_history("local", "enable").unwrap();
        db.add_history("local", "conf t").unwrap();
        db.add_history("local", "").unwrap(); // empty lines are skipped
        db.add_history("core", "show ip route").unwrap();

        assert_eq!(
            db.load_history("local", 10).unwrap(),
            vec!["enable".to_string(), "conf t".to_string()]
        );
        assert_eq!(
            db.load_history("local", 1).unwrap(),
            vec!["conf t".to_string()]
        );
        assert_eq!(
            db.load_history("core", 10).unwrap(),
            vec!["show ip route".to_string()]
        );
    }

    #[test]
    fn test_devices_upsert_and_delete() {
        let db = test_db();
        db.save_device("core", "10.0.0.1", 22).unwrap();
        db.save_device("core", "10.0.0.2", 23).unwrap(); // upsert, not duplicate
        db.save_device("edge", "10.0.0.9", 23).unwrap();

        let core = db.get_device("core").unwrap().unwrap();
        assert_eq!((core.host.as_str(), core.port), ("10.0.0.2", 23));

        let names: Vec<String> = db.list_devices().unwrap().iter().map(|d| d.name.clone()).collect();
        assert_eq!(names, vec!["core".to_string(), "edge".to_string()]);

        assert!(db.delete_device("core").unwrap());
        assert!(!db.delete_device("core").unwrap()); // already gone
        assert!(db.get_device("core").unwrap().is_none());
    }

    #[test]
    fn test_migrations_applied_once() {
        let db = test_db();
        let again = Database::open().unwrap(); // re-open same file
        let n: i64 = again
            .conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        drop(db);
    }
}