use super::schema::*;
use super::Database;
use anyhow::Result;

/// Parameters for creating a new workspace.
pub struct CreateWorkspaceParams {
    pub name: String,
    pub mount_path: Option<String>,
    pub mount_read_only: bool,
    pub is_git_repo: bool,
    pub branch_name: Option<String>,
    pub base_branch: Option<String>,
    pub initial_prompt: Option<String>,
    pub sandbox_cpus: i32,
    pub sandbox_memory_mb: i64,
    pub sandbox_disk_mb: i64,
    pub allowed_hosts: Option<Vec<String>>,
    pub secrets_config: Option<String>,
    pub cloned_from_id: Option<i64>,
}

impl Database {
    /// Insert a new workspace, return its id.
    pub fn create_workspace(&self, params: CreateWorkspaceParams) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let allowed_hosts_json = params
            .allowed_hosts
            .map(|h| serde_json::to_string(&h).unwrap());
        let max_order: i32 = conn
            .query_row("SELECT COALESCE(MAX(tab_order), -1) FROM workspaces", [], |r| {
                r.get(0)
            })
            .unwrap_or(-1);

        conn.execute(
            "INSERT INTO workspaces (name, mount_path, mount_read_only, is_git_repo, branch_name, base_branch,
             initial_prompt, sandbox_cpus, sandbox_memory_mb, sandbox_disk_mb,
             allowed_hosts, secrets_config, cloned_from_id, tab_order)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            rusqlite::params![
                params.name,
                params.mount_path,
                params.mount_read_only,
                params.is_git_repo,
                params.branch_name,
                params.base_branch,
                params.initial_prompt,
                params.sandbox_cpus,
                params.sandbox_memory_mb,
                params.sandbox_disk_mb,
                allowed_hosts_json,
                params.secrets_config,
                params.cloned_from_id,
                max_order + 1,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// List all non-archived workspaces ordered by tab_order.
    pub fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, status, cloned_from_id, initial_prompt, tab_order,
             last_opened_at, is_unread, mount_path, mount_read_only, is_git_repo,
             branch_name, base_branch, git_status, diff_stats, pr_number, pr_url,
             sandbox_cpus, sandbox_memory_mb, sandbox_disk_mb, allowed_hosts,
             secrets_config, sandbox_instance_dir, sandbox_checkpoint_name,
             created_at, stopped_at, deleting_at, port_mappings, expose_host_ports
             FROM workspaces
             WHERE status != 'archived'
             ORDER BY tab_order ASC",
        )?;

        let rows = stmt.query_map([], |row| {
            let git_status_str: Option<String> = row.get(13)?;
            let diff_stats_str: Option<String> = row.get(14)?;
            let allowed_hosts_str: Option<String> = row.get(20)?;
            let port_mappings_str: Option<String> = row.get(27)?;
            let expose_host_str: Option<String> = row.get(28)?;

            Ok(Workspace {
                id: row.get(0)?,
                name: row.get(1)?,
                status: WorkspaceStatus::from_str(&row.get::<_, String>(2)?),
                cloned_from_id: row.get(3)?,
                initial_prompt: row.get(4)?,
                tab_order: row.get(5)?,
                last_opened_at: row.get(6)?,
                is_unread: row.get(7)?,
                mount_path: row.get(8)?,
                mount_read_only: row.get(9)?,
                is_git_repo: row.get(10)?,
                branch_name: row.get(11)?,
                base_branch: row.get(12)?,
                git_status: git_status_str
                    .and_then(|s| serde_json::from_str(&s).ok()),
                diff_stats: diff_stats_str
                    .and_then(|s| serde_json::from_str(&s).ok()),
                pr_number: row.get(15)?,
                pr_url: row.get(16)?,
                sandbox_cpus: row.get(17)?,
                sandbox_memory_mb: row.get(18)?,
                sandbox_disk_mb: row.get(19)?,
                allowed_hosts: allowed_hosts_str
                    .and_then(|s| serde_json::from_str(&s).ok()),
                secrets_config: row.get(21)?,
                port_mappings: port_mappings_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default(),
                expose_host_ports: expose_host_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default(),
                sandbox_instance_dir: row.get(22)?,
                sandbox_checkpoint_name: row.get(23)?,
                created_at: row.get(24)?,
                stopped_at: row.get(25)?,
                deleting_at: row.get(26)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Update workspace status.
    pub fn update_workspace_status(&self, id: i64, status: WorkspaceStatus) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE workspaces SET status = ?1, stopped_at = CASE WHEN ?1 IN ('stopped', 'archived') THEN datetime('now') ELSE stopped_at END WHERE id = ?2",
            rusqlite::params![status.as_str(), id],
        )?;
        Ok(())
    }

    /// Update cached git status for a workspace.
    #[allow(dead_code)]
    pub fn update_git_status(
        &self,
        id: i64,
        git_status: &GitStatus,
        diff_stats: &DiffStats,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE workspaces SET git_status = ?1, diff_stats = ?2 WHERE id = ?3",
            rusqlite::params![
                serde_json::to_string(git_status)?,
                serde_json::to_string(diff_stats)?,
                id,
            ],
        )?;
        Ok(())
    }

    // ── Port mappings ─────────────────────────────────────────────

    /// Get port mappings for a workspace.
    pub fn get_port_mappings(&self, workspace_id: i64) -> Result<Vec<PortMapping>> {
        let conn = self.conn.lock().unwrap();
        let json: String = conn.query_row(
            "SELECT COALESCE(port_mappings, '[]') FROM workspaces WHERE id = ?1",
            [workspace_id],
            |row| row.get(0),
        )?;
        Ok(serde_json::from_str(&json).unwrap_or_default())
    }

    /// Set port mappings for a workspace.
    pub fn set_port_mappings(&self, workspace_id: i64, mappings: &[PortMapping]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let json = serde_json::to_string(mappings)?;
        conn.execute(
            "UPDATE workspaces SET port_mappings = ?1 WHERE id = ?2",
            rusqlite::params![json, workspace_id],
        )?;
        Ok(())
    }

    /// Add a single port mapping to a workspace.
    pub fn add_port_mapping(&self, workspace_id: i64, mapping: &PortMapping) -> Result<()> {
        let mut mappings = self.get_port_mappings(workspace_id)?;
        // Don't add duplicate guest ports
        if mappings.iter().any(|m| m.guest_port == mapping.guest_port) {
            anyhow::bail!("Guest port {} is already mapped", mapping.guest_port);
        }
        mappings.push(mapping.clone());
        self.set_port_mappings(workspace_id, &mappings)
    }

    /// Remove a port mapping by guest port.
    pub fn remove_port_mapping(&self, workspace_id: i64, guest_port: u16) -> Result<()> {
        let mut mappings = self.get_port_mappings(workspace_id)?;
        mappings.retain(|m| m.guest_port != guest_port);
        self.set_port_mappings(workspace_id, &mappings)
    }

    // ── Expose host ports ────────────────────────────────────────

    /// Get host ports exposed to a workspace's sandbox.
    pub fn get_expose_host_ports(&self, workspace_id: i64) -> Result<Vec<ExposeHostPort>> {
        let conn = self.conn.lock().unwrap();
        let json: String = conn.query_row(
            "SELECT COALESCE(expose_host_ports, '[]') FROM workspaces WHERE id = ?1",
            [workspace_id],
            |row| row.get(0),
        )?;
        Ok(serde_json::from_str(&json).unwrap_or_default())
    }

    /// Set host ports exposed to a workspace's sandbox.
    pub fn set_expose_host_ports(&self, workspace_id: i64, mappings: &[ExposeHostPort]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let json = serde_json::to_string(mappings)?;
        conn.execute(
            "UPDATE workspaces SET expose_host_ports = ?1 WHERE id = ?2",
            rusqlite::params![json, workspace_id],
        )?;
        Ok(())
    }

    /// Add a single expose-host mapping to a workspace.
    pub fn add_expose_host_port(&self, workspace_id: i64, mapping: &ExposeHostPort) -> Result<()> {
        let mut mappings = self.get_expose_host_ports(workspace_id)?;
        if mappings.iter().any(|m| m.host_port == mapping.host_port) {
            anyhow::bail!("Host port {} is already exposed", mapping.host_port);
        }
        mappings.push(mapping.clone());
        self.set_expose_host_ports(workspace_id, &mappings)
    }

    /// Remove an expose-host mapping by host port.
    pub fn remove_expose_host_port(&self, workspace_id: i64, host_port: u16) -> Result<()> {
        let mut mappings = self.get_expose_host_ports(workspace_id)?;
        mappings.retain(|m| m.host_port != host_port);
        self.set_expose_host_ports(workspace_id, &mappings)
    }

    /// Soft-delete a workspace (set deleting_at).
    #[allow(dead_code)]
    pub fn begin_delete_workspace(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE workspaces SET deleting_at = datetime('now') WHERE id = ?1",
            [id],
        )?;
        Ok(())
    }

    /// Delete a workspace and its saved tabs.
    pub fn delete_workspace(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM terminal_tabs WHERE workspace_id = ?1", [id])?;
        conn.execute("DELETE FROM workspaces WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Save a checkpointed tab to the database.
    pub fn save_checkpointed_tab(
        &self,
        workspace_id: i64,
        label: &str,
        agent_id: i64,
        checkpoint_name: &str,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO terminal_tabs (workspace_id, label, agent_id, checkpoint_name)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![workspace_id, label, agent_id, checkpoint_name],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Load checkpointed tabs for a workspace.
    pub fn load_checkpointed_tabs(&self, workspace_id: i64) -> Result<Vec<SavedTab>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, label, agent_id, checkpoint_name
             FROM terminal_tabs
             WHERE workspace_id = ?1 AND checkpoint_name IS NOT NULL
             ORDER BY tab_order ASC",
        )?;
        let rows = stmt.query_map([workspace_id], |row| {
            Ok(SavedTab {
                id: row.get(0)?,
                label: row.get(1)?,
                agent_id: row.get(2)?,
                checkpoint_name: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Delete a saved checkpointed tab.
    pub fn delete_checkpointed_tab(&self, tab_db_id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM terminal_tabs WHERE id = ?1", [tab_db_id])?;
        Ok(())
    }

    /// List all registered agents.
    /// Static config (required_secrets, auth_setup_script) is filled from
    /// embedded AgentConfigs — call `.with_config()` on results.
    pub fn list_agents(&self) -> Result<Vec<Agent>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, display_name, command, icon, color, is_builtin, tab_order
             FROM agents ORDER BY tab_order ASC",
        )?;
        let configs = crate::agents::builtin_agents();
        let rows = stmt.query_map([], |row| {
            Ok(Agent {
                id: row.get(0)?,
                name: row.get(1)?,
                display_name: row.get(2)?,
                command: row.get(3)?,
                icon: row.get(4)?,
                color: row.get(5)?,
                is_builtin: row.get(6)?,
                tab_order: row.get(7)?,
                required_secrets: vec![],
            })
        })?;
        let agents: Vec<Agent> = rows
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|a| a.with_config(&configs))
            .collect();
        Ok(agents)
    }

    /// Get app settings.
    #[allow(dead_code)]
    pub fn get_settings(&self) -> Result<Settings> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, last_active_workspace_id, theme, terminal_font_family,
                    terminal_font_size, sidebar_width, review_panel_width,
                    confirm_on_quit, default_agent_id,
                    sandbox_cpus, sandbox_memory_mb, sandbox_disk_mb, allowed_hosts
             FROM settings WHERE id = 1",
            [],
            |row| {
                let allowed_hosts_str: Option<String> = row.get(12)?;
                Ok(Settings {
                    last_active_workspace_id: row.get(1)?,
                    theme: row.get(2)?,
                    terminal_font_family: row.get(3)?,
                    terminal_font_size: row.get(4)?,
                    sidebar_width: row.get(5)?,
                    review_panel_width: row.get(6)?,
                    confirm_on_quit: row.get(7)?,
                    default_agent_id: row.get(8)?,
                    sandbox_cpus: row.get(9)?,
                    sandbox_memory_mb: row.get(10)?,
                    sandbox_disk_mb: row.get(11)?,
                    allowed_hosts: allowed_hosts_str
                        .and_then(|s| serde_json::from_str(&s).ok()),
                })
            },
        )
        .map_err(Into::into)
    }

    pub fn update_sandbox_settings(&self, cpus: i32, memory_mb: i64, disk_mb: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE settings SET sandbox_cpus = ?1, sandbox_memory_mb = ?2, sandbox_disk_mb = ?3 WHERE id = 1",
            rusqlite::params![cpus, memory_mb, disk_mb],
        )?;
        Ok(())
    }

    pub fn update_default_agent(&self, agent_id: Option<i64>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE settings SET default_agent_id = ?1 WHERE id = 1",
            rusqlite::params![agent_id],
        )?;
        Ok(())
    }

    /// Get the workspace name that a given workspace was cloned from.
    pub fn get_cloned_from_name(&self, cloned_from_id: i64) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let name = conn
            .query_row(
                "SELECT name FROM workspaces WHERE id = ?1",
                [cloned_from_id],
                |row| row.get(0),
            )
            .ok();
        Ok(name)
    }

    // ── Secret CRUD ─────────────────────────────────────────────────

    /// Save (or update) a secret. The plaintext value is encrypted before storage.
    pub fn save_secret(
        &self,
        env_var: &str,
        label: &str,
        plaintext: &str,
        hosts: &[String],
    ) -> Result<()> {
        let encrypted = super::crypto::encrypt(plaintext.as_bytes(), &self.encryption_key)?;
        let hosts_json = serde_json::to_string(hosts)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO secrets (env_var, label, encrypted_value, hosts, updated_at)
             VALUES (?1, ?2, ?3, ?4, datetime('now'))
             ON CONFLICT(env_var) DO UPDATE SET
                label = excluded.label,
                encrypted_value = excluded.encrypted_value,
                hosts = excluded.hosts,
                updated_at = datetime('now')",
            rusqlite::params![env_var, label, encrypted, hosts_json],
        )?;
        Ok(())
    }

    /// Decrypt and return the secret value for an env var, if it exists.
    pub fn get_secret_value(&self, env_var: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let blob: Option<Vec<u8>> = conn
            .query_row(
                "SELECT encrypted_value FROM secrets WHERE env_var = ?1",
                [env_var],
                |row| row.get(0),
            )
            .ok();
        match blob {
            Some(b) => {
                let plaintext = super::crypto::decrypt(&b, &self.encryption_key)?;
                Ok(Some(String::from_utf8(plaintext)?))
            }
            None => Ok(None),
        }
    }

    /// Remove a secret by env var name.
    pub fn remove_secret(&self, env_var: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM secrets WHERE env_var = ?1", [env_var])?;
        Ok(())
    }

    /// Check if a secret exists for the given env var.
    pub fn has_secret(&self, env_var: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM secrets WHERE env_var = ?1)",
            [env_var],
            |row| row.get(0),
        )?;
        Ok(exists)
    }

    /// List all secrets (metadata only, no decryption).
    pub fn list_secrets(&self) -> Result<Vec<Secret>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, env_var, label, hosts, auth_method, oauth_expires_at
             FROM secrets ORDER BY env_var ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            let hosts_str: Option<String> = row.get(3)?;
            let hosts: Vec<String> = hosts_str
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            Ok(Secret {
                id: row.get(0)?,
                env_var: row.get(1)?,
                label: row.get(2)?,
                hosts,
                auth_method: row.get::<_, String>(4).unwrap_or_else(|_| "api_key".into()),
                oauth_expires_at: row.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Get the host patterns for a secret.
    pub fn get_secret_hosts(&self, env_var: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let hosts_str: Option<String> = conn
            .query_row(
                "SELECT hosts FROM secrets WHERE env_var = ?1",
                [env_var],
                |row| row.get(0),
            )
            .ok()
            .flatten();
        Ok(hosts_str
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default())
    }

    // ── OAuth secrets ───────────────────────────────────────────────

    /// Save an OAuth-obtained secret (access token + refresh token + id token).
    pub fn save_oauth_secret(
        &self,
        env_var: &str,
        label: &str,
        access_token: &str,
        refresh_token: Option<&str>,
        id_token: Option<&str>,
        expires_at: Option<&str>,
        hosts: &[String],
    ) -> Result<()> {
        let encrypted_value =
            super::crypto::encrypt(access_token.as_bytes(), &self.encryption_key)?;
        let encrypted_refresh = match refresh_token {
            Some(rt) => Some(super::crypto::encrypt(rt.as_bytes(), &self.encryption_key)?),
            None => None,
        };
        let encrypted_id = match id_token {
            Some(it) => Some(super::crypto::encrypt(it.as_bytes(), &self.encryption_key)?),
            None => None,
        };
        let hosts_json = serde_json::to_string(hosts)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO secrets (env_var, label, encrypted_value, hosts, auth_method,
                                  oauth_refresh_token, oauth_id_token, oauth_expires_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'oauth', ?5, ?6, ?7, datetime('now'))
             ON CONFLICT(env_var) DO UPDATE SET
                label = excluded.label,
                encrypted_value = excluded.encrypted_value,
                hosts = excluded.hosts,
                auth_method = 'oauth',
                oauth_refresh_token = excluded.oauth_refresh_token,
                oauth_id_token = excluded.oauth_id_token,
                oauth_expires_at = excluded.oauth_expires_at,
                updated_at = datetime('now')",
            rusqlite::params![
                env_var,
                label,
                encrypted_value,
                hosts_json,
                encrypted_refresh,
                encrypted_id,
                expires_at,
            ],
        )?;
        Ok(())
    }

    /// Get the decrypted refresh token for an OAuth secret.
    pub fn get_oauth_refresh_token(&self, env_var: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let blob: Option<Vec<u8>> = conn
            .query_row(
                "SELECT oauth_refresh_token FROM secrets WHERE env_var = ?1",
                [env_var],
                |row| row.get(0),
            )
            .ok()
            .flatten();
        match blob {
            Some(b) => {
                let plaintext = super::crypto::decrypt(&b, &self.encryption_key)?;
                Ok(Some(String::from_utf8(plaintext)?))
            }
            None => Ok(None),
        }
    }

    /// Get the decrypted id_token for an OAuth secret.
    pub fn get_oauth_id_token(&self, env_var: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let blob: Option<Vec<u8>> = conn
            .query_row(
                "SELECT oauth_id_token FROM secrets WHERE env_var = ?1",
                [env_var],
                |row| row.get(0),
            )
            .ok()
            .flatten();
        match blob {
            Some(b) => {
                let plaintext = super::crypto::decrypt(&b, &self.encryption_key)?;
                Ok(Some(String::from_utf8(plaintext)?))
            }
            None => Ok(None),
        }
    }

    /// Get the auth_method for a secret.
    pub fn get_secret_auth_method(&self, env_var: &str) -> Result<String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT auth_method FROM secrets WHERE env_var = ?1",
            [env_var],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    /// Get the OAuth expiry time for a secret.
    pub fn get_oauth_expires_at(&self, env_var: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT oauth_expires_at FROM secrets WHERE env_var = ?1",
                [env_var],
                |row| row.get(0),
            )
            .ok()
            .flatten())
    }
}
