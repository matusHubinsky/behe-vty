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


use crate::vault;
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

/// Lifts a vault (encryption) error into the rusqlite error type used by
/// the whole Database API.
fn db_err(msg: String) -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName(msg)
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

/// The schema version this build expects. `PRAGMA user_version` in the
/// database file records which version it is on.
const SCHEMA_VERSION: i64 = 1;

/// The whole current schema, applied in one shot to fresh (or pre-versioning)
/// databases. During development schema changes go directly into this file.
const BASELINE_SCHEMA: &str = include_str!("../migrations/001-schema.sql");

/// Migrations for databases created by older *released* versions, applied in
/// order. Empty until the first schema change after a release.
const MIGRATIONS: &[(i64, &str)] = &[];

impl Database {
    pub fn open() -> Result<Self> {
        Self::open_at(&db_path())
    }

    fn open_at(path: &std::path::Path) -> Result<Self> {
        let mut conn = Connection::open(path)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;

        if version == 0 {
            // Fresh database (or one from before version tracking): apply the
            // whole baseline schema in one shot. Every statement is idempotent,
            // so re-running it on an old database is safe.
            conn.execute_batch(BASELINE_SCHEMA)?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        } else {
            // Existing database: apply only the migrations it is missing.
            for (migration_version, sql) in MIGRATIONS {
                if *migration_version > version {
                    let tx = conn.transaction()?;
                    tx.execute_batch(sql)?;
                    tx.pragma_update(None, "user_version", *migration_version)?;
                    tx.commit()?;
                }
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

    // ---------------- credentials

    /// Saves (or updates) a login, sealing the password with the master
    /// password. Returns the credential id.
    pub fn save_credential(
        &self,
        master: &str,
        username: &str,
        password: &str,
    ) -> Result<i64> {
        let salt = vault::random_salt();
        let key = vault::derive_key(master, &salt).map_err(db_err)?;
        let (nonce, ciphertext) = vault::seal(&key, password).map_err(db_err)?;

        self.conn.execute(
            "INSERT INTO credentials (username, password, nonce, salt)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(username) DO UPDATE SET
                 password = ?2, nonce = ?3, salt = ?4",
            params![username, ciphertext, nonce, salt],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Links a saved login to a saved device (idempotent).
    pub fn link_credential(&self, device: &str, credential: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO device_credentials (device, credential)
             VALUES (?1, ?2)",
            params![device, credential],
        )?;
        Ok(())
    }

    /// The `(username, password)` saved for `device`, decrypted with the
    /// master password. `Err` means the master password is wrong (or the row
    /// is corrupted); `Ok(None)` means the device has no saved login.
    pub fn credential_for_device(
        &self,
        master: &str,
        device: &str,
    ) -> Result<Option<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.username, c.password, c.nonce, c.salt
             FROM credentials c
             JOIN device_credentials d ON d.credential = c.id
             WHERE d.device = ?1
             ORDER BY c.id DESC
             LIMIT 1",
        )?;

        let mut rows = stmt.query(params![device])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };

        let username: String = row.get(0)?;
        let ciphertext: Vec<u8> = row.get(1)?;
        let nonce: Vec<u8> = row.get(2)?;
        let salt: Vec<u8> = row.get(3)?;

        let key = vault::derive_key(master, &salt).map_err(db_err)?;
        let password = vault::open(&key, &nonce, &ciphertext).map_err(db_err)?;
        Ok(Some((username, password)))
    }

    /// Whether any encrypted login is stored.
    pub fn has_credentials(&self) -> Result<bool> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM credentials", [], |r| r.get(0))?;
        Ok(n > 0)
    }

    /// Whether `master` decrypts the stored credentials. Vacuously true
    /// when nothing is stored yet.
    pub fn verify_master(&self, master: &str) -> Result<bool> {
        let mut stmt = self
            .conn
            .prepare("SELECT password, nonce, salt FROM credentials ORDER BY id LIMIT 1")?;
        let mut rows = stmt.query([])?;
        let Some(row) = rows.next()? else {
            return Ok(true);
        };

        let ciphertext: Vec<u8> = row.get(0)?;
        let nonce: Vec<u8> = row.get(1)?;
        let salt: Vec<u8> = row.get(2)?;

        let key = vault::derive_key(master, &salt).map_err(db_err)?;
        Ok(vault::open(&key, &nonce, &ciphertext).is_ok())
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
    fn test_old_database_upgraded_in_place() {
        let path = test_db_path();
        {
            // A database from before version tracking: old devices layout
            // plus the retired schema_migrations bookkeeping table.
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_migrations (version TEXT PRIMARY KEY);
                 INSERT INTO schema_migrations VALUES ('001_init'), ('002_devices_extra');
                 CREATE TABLE history (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     command TEXT NOT NULL,
                     device TEXT NOT NULL DEFAULT 'local',
                     created_at TEXT NOT NULL DEFAULT (datetime('now'))
                 );
                 CREATE TABLE devices (
                     name TEXT PRIMARY KEY,
                     host TEXT NOT NULL,
                     port INTEGER NOT NULL,
                     protocol TEXT NOT NULL DEFAULT 'ssh',
                     vendor TEXT NOT NULL DEFAULT 'cisco',
                     username TEXT NOT NULL DEFAULT ''
                 );
                 INSERT INTO devices (name, host, port) VALUES ('core', '10.0.0.1', 22);",
            )
            .unwrap();
        }

        let db = Database::open_at(&path).unwrap();

        // Existing data survives.
        let core = db.get_device("core").unwrap().unwrap();
        assert_eq!(core.host, "10.0.0.1");

        // The baseline added the credentials tables and recorded the version.
        let version: i64 = db.conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, 1);
        let tables: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'credentials'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 1);
        let retired: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(retired, 0);
    }

    #[test]
    fn test_schema_version_stable_across_reopen() {
        let path = test_db_path();
        let db = Database::open_at(&path).unwrap();
        let version = |d: &Database| -> i64 {
            d.conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(version(&db), 1);

        let again = Database::open_at(&path).unwrap(); // re-open same file
        assert_eq!(version(&again), 1, "re-opening must not re-apply the schema");
        drop(db);
    }

    #[test]
    fn test_credentials_roundtrip_wrong_master_and_cascade() {
        let db = test_db();
        db.save_device(&Device {
            name: "core".to_string(),
            host: "10.0.0.1".to_string(),
            port: 22,
            protocol: "ssh".to_string(),
            vendor: "cisco".to_string(),
            username: String::new(),
        })
        .unwrap();

        let id = db.save_credential("master", "admin", "admin123").unwrap();
        db.link_credential("core", id).unwrap();
        db.link_credential("core", id).unwrap(); // idempotent

        let (user, pass) = db.credential_for_device("master", "core")
            .unwrap()
            .expect("credential must be linked");
        assert_eq!((user.as_str(), pass.as_str()), ("admin", "admin123"));

        // No saved login for an unknown device.
        assert!(db.credential_for_device("master", "edge").unwrap().is_none());

        // Wrong master password must fail, not return garbage.
        assert!(db.credential_for_device("wrong", "core").is_err());

        // Updating a login reuses the same row.
        let id2 = db.save_credential("master", "admin", "newpass").unwrap();
        assert_eq!(id, id2);
        let (_, pass) = db.credential_for_device("master", "core").unwrap().unwrap();
        assert_eq!(pass, "newpass");

        // Deleting a device cascades to the link, not to the login.
        db.delete_device("core").unwrap();
        assert!(db.credential_for_device("master", "core").unwrap().is_none());
    }

    #[test]
    fn test_has_credentials_and_verify_master() {
        let db = test_db();

        assert!(!db.has_credentials().unwrap());
        assert!(db.verify_master("anything").unwrap()); // nothing stored yet

        db.save_credential("master", "admin", "admin123").unwrap();
        assert!(db.has_credentials().unwrap());
        assert!(db.verify_master("master").unwrap());
        assert!(!db.verify_master("wrong").unwrap());
    }
}