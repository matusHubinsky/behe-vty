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
    pub protocol: String, // "serial" | "ssh" | "telnet"
    pub vendor: String,   // "cisco" | "juniper" | "mikrotik"
    pub username: String,
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
const MIGRATIONS: &[(&str, &str)] = &[
    ("001_init", include_str!("../migrations/001-init.sql")),
    ("002_devices_extra", include_str!("../migrations/002-devices-extra.sql")),
];

impl Database {
    pub fn open() -> Result<Self> {
        Self::open_at(&db_path())
    }

    fn open_at(path: &std::path::Path) -> Result<Self> {
        let mut conn = Connection::open(path)?;
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

    /// The most-used commands from the history of `device` and of every
    /// saved device of the same `vendor`, most frequent first (ties: most
    /// recent first). Used to rank autocomplete suggestions.
    pub fn frequent_commands(&self, device: &str, vendor: &str, limit: usize) -> Result<Vec<(String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT command, COUNT(*) FROM history
             WHERE device = ?1
                OR device IN (SELECT name FROM devices WHERE vendor = ?2)
             GROUP BY command
             ORDER BY COUNT(*) DESC, MAX(id) DESC
             LIMIT ?3",
        )?;

        let rows = stmt.query_map(params![device, vendor, limit as i64], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        rows.collect()
    }

    // ---------------- devices

    pub fn save_device(&self, d: &Device) -> Result<()> {
        self.conn.execute(
            "INSERT INTO devices (name, host, port, protocol, vendor, username)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(name) DO UPDATE SET
                 host = ?2, port = ?3, protocol = ?4, vendor = ?5, username = ?6",
            params![d.name, d.host, d.port, d.protocol, d.vendor, d.username],
        )?;
        Ok(())
    }

    #[allow(dead_code)] // used by tests; part of the device CRUD API
    pub fn get_device(&self, name: &str) -> Result<Option<Device>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, host, port, protocol, vendor, username FROM devices WHERE name = ?1")?;

        let mut rows = stmt.query_map(params![name], |row| {
            Ok(Device {
                name: row.get(0)?,
                host: row.get(1)?,
                port: row.get(2)?,
                protocol: row.get(3)?,
                vendor: row.get(4)?,
                username: row.get(5)?,
            })
        })?;

        rows.next().transpose()
    }

    pub fn list_devices(&self) -> Result<Vec<Device>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, host, port, protocol, vendor, username FROM devices ORDER BY name")?;

        let rows = stmt.query_map([], |row| {
            Ok(Device {
                name: row.get(0)?,
                host: row.get(1)?,
                port: row.get(2)?,
                protocol: row.get(3)?,
                vendor: row.get(4)?,
                username: row.get(5)?,
            })
        })?;

        rows.collect()
    }

    #[allow(dead_code)] // used by tests; part of the device CRUD API
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
        Database::open_at(&test_db_path()).unwrap()
    }

    fn test_db_path() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "behe-vty-test-{}-{}.db",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_file(&path);
        path
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
        let dev = |name: &str, host: &str, port: u16| Device {
            name: name.to_string(),
            host: host.to_string(),
            port,
            protocol: "ssh".to_string(),
            vendor: "cisco".to_string(),
            username: String::new(),
        };
        db.save_device(&dev("core", "10.0.0.1", 22)).unwrap();
        db.save_device(&dev("core", "10.0.0.2", 23)).unwrap(); // upsert, not duplicate
        db.save_device(&dev("edge", "10.0.0.9", 23)).unwrap();

        let core = db.get_device("core").unwrap().unwrap();
        assert_eq!((core.host.as_str(), core.port), ("10.0.0.2", 23));

        let names: Vec<String> = db.list_devices().unwrap().iter().map(|d| d.name.clone()).collect();
        assert_eq!(names, vec!["core".to_string(), "edge".to_string()]);

        assert!(db.delete_device("core").unwrap());
        assert!(!db.delete_device("core").unwrap()); // already gone
        assert!(db.get_device("core").unwrap().is_none());
    }

    #[test]
    fn test_frequent_commands_rank_by_usage() {
        let db = test_db();
        let dev = |name: &str, vendor: &str| Device {
            name: name.to_string(),
            host: "10.0.0.1".to_string(),
            port: 22,
            protocol: "ssh".to_string(),
            vendor: vendor.to_string(),
            username: String::new(),
        };
        db.save_device(&dev("core", "cisco")).unwrap();
        db.save_device(&dev("edge", "cisco")).unwrap();
        db.save_device(&dev("jbox", "juniper")).unwrap();

        db.add_history("core", "show ip route").unwrap();
        db.add_history("core", "show ip route").unwrap();
        db.add_history("edge", "show ip interface brief").unwrap(); // same vendor
        db.add_history("jbox", "show interface terse").unwrap(); // other vendor
        db.add_history("local", "enable").unwrap(); // unrelated device

        let freq = db.frequent_commands("core", "cisco", 10).unwrap();
        assert_eq!(
            freq,
            vec![
                ("show ip route".to_string(), 2),
                ("show ip interface brief".to_string(), 1),
            ]
        );

        // For the local simulator the same-vendor device history counts too.
        let local = db.frequent_commands("local", "cisco", 10).unwrap();
        assert_eq!(
            local,
            vec![
                ("show ip route".to_string(), 2),
                ("enable".to_string(), 1),
                ("show ip interface brief".to_string(), 1),
            ]
        );

        // Other vendors' history stays out.
        let juniper = db.frequent_commands("local", "juniper", 10).unwrap();
        assert_eq!(
            juniper,
            vec![
                ("enable".to_string(), 1),
                ("show interface terse".to_string(), 1),
            ]
        );
    }

    #[test]
    fn test_migrations_applied_once() {
        let path = test_db_path();
        let db = Database::open_at(&path).unwrap();
        let count = |d: &Database| -> i64 {
            d.conn
                .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
                .unwrap()
        };
        let before = count(&db);
        let again = Database::open_at(&path).unwrap(); // re-open same file
        let after = count(&again);
        assert_eq!(before, after, "re-opening must not re-apply migrations");
        assert!(after >= 1);
        drop(db);
    }
}