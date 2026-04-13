pub mod crypto;
mod queries;
mod schema;

pub use queries::*;
pub use schema::*;

use crate::agents;
use anyhow::Result;
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Database handle wrapping a SQLite connection.
/// All access goes through this to ensure thread safety.
pub struct Database {
    conn: Arc<Mutex<Connection>>,
    pub(crate) encryption_key: [u8; 32],
}

impl Database {
    /// Open (or create) the database at the default app data path.
    pub fn open() -> Result<Self> {
        let path = Self::db_path();
        std::fs::create_dir_all(path.parent().unwrap())?;

        let encryption_key = crypto::load_or_create_key()?;

        let conn = Connection::open(&path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

        // Set 0600 on the database file
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }

        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
            encryption_key,
        };
        db.migrate()?;
        db.sync_builtin_agents()?;
        db.migrate_legacy_config()?;
        Ok(db)
    }

    /// Open an in-memory database (for testing).
    #[allow(dead_code)]
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
            encryption_key: crypto::ephemeral_key(),
        };
        db.migrate()?;
        Ok(db)
    }

    /// Run versioned migrations.
    fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS _migrations (version INTEGER PRIMARY KEY);",
        )?;

        let current: i32 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM _migrations",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);

        if current < 1 {
            conn.execute_batch(include_str!("../../migrations/001_initial.sql"))?;
            conn.execute("INSERT INTO _migrations (version) VALUES (1)", [])?;
        }

        if current < 2 {
            conn.execute_batch(include_str!("../../migrations/002_auto_launch_agent.sql"))
                .ok(); // ignore if column already exists
            conn.execute("INSERT OR REPLACE INTO _migrations (version) VALUES (2)", [])?;
        }

        Ok(())
    }

    /// Sync built-in agents from embedded configs.
    /// Ensures DB rows exist (for stable IDs and FKs) and updates
    /// display fields. Static config (secrets, install steps) is
    /// read from AgentConfig at runtime, not stored in the DB.
    fn sync_builtin_agents(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let configs = agents::builtin_agents();

        // Remove stale built-in agents no longer in config
        let names: Vec<&str> = configs.iter().map(|c| c.name).collect();
        let placeholders: String = names.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let query = format!(
            "DELETE FROM agents WHERE is_builtin = 1 AND name NOT IN ({placeholders})"
        );
        let params: Vec<&dyn rusqlite::types::ToSql> =
            names.iter().map(|n| n as &dyn rusqlite::types::ToSql).collect();
        conn.execute(&query, params.as_slice())?;

        for cfg in &configs {
            conn.execute(
                "INSERT INTO agents (name, display_name, command, icon, color, is_builtin, tab_order)
                 VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)
                 ON CONFLICT(name) DO UPDATE SET
                    display_name = excluded.display_name,
                    command = excluded.command,
                    icon = excluded.icon,
                    color = excluded.color",
                rusqlite::params![
                    cfg.name,
                    cfg.display_name,
                    cfg.command,
                    cfg.icon,
                    cfg.color,
                    cfg.tab_order,
                ],
            )?;
        }
        Ok(())
    }

    /// One-time import of secrets from legacy shuru.json config file.
    fn migrate_legacy_config(&self) -> Result<()> {
        let path = Self::legacy_config_path();
        if !path.exists() {
            return Ok(());
        }

        let contents = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };

        let json: serde_json::Value = match serde_json::from_str(&contents) {
            Ok(v) => v,
            Err(_) => return Ok(()),
        };

        if let Some(api_keys) = json.get("api_keys").and_then(|v| v.as_object()) {
            for (env_var, entry) in api_keys {
                let label = entry
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or(env_var);
                let value = match entry.get("value").and_then(|v| v.as_str()) {
                    Some(v) if !v.is_empty() => v,
                    _ => continue,
                };
                let hosts = default_hosts_for_env(env_var);
                // Ignore errors — best-effort migration
                let _ = self.save_secret(env_var, label, value, &hosts);
            }
        }

        // Rename to .bak so we don't re-import
        let _ = std::fs::rename(&path, path.with_extension("json.bak"));
        Ok(())
    }

    /// Get a reference to the connection mutex for queries.
    #[allow(dead_code)]
    pub fn conn(&self) -> &Arc<Mutex<Connection>> {
        &self.conn
    }

    fn db_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("superhq")
            .join("superhq.db")
    }

    fn legacy_config_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("superhq")
            .join("shuru.json")
    }
}

/// Default host patterns for well-known env vars.
fn default_hosts_for_env(env_var: &str) -> Vec<String> {
    match env_var {
        "ANTHROPIC_API_KEY" => vec!["api.anthropic.com".into()],
        "OPENAI_API_KEY" => vec!["api.openai.com".into()],
        _ => vec![],
    }
}
