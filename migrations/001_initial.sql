-- SuperHQ schema

CREATE TABLE IF NOT EXISTS workspaces (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'stopped',
    cloned_from_id INTEGER REFERENCES workspaces(id) ON DELETE SET NULL,
    initial_prompt TEXT,
    tab_order INTEGER NOT NULL DEFAULT 0,
    last_opened_at TEXT,
    is_unread INTEGER NOT NULL DEFAULT 0,

    -- Mount
    mount_path TEXT,
    mount_read_only INTEGER NOT NULL DEFAULT 1,
    is_git_repo INTEGER NOT NULL DEFAULT 0,
    branch_name TEXT,
    base_branch TEXT,

    -- Git cache (JSON)
    git_status TEXT,
    diff_stats TEXT,
    pr_number INTEGER,
    pr_url TEXT,

    -- Sandbox config
    sandbox_cpus INTEGER NOT NULL DEFAULT 2,
    sandbox_memory_mb INTEGER NOT NULL DEFAULT 8192,
    sandbox_disk_mb INTEGER NOT NULL DEFAULT 16384,
    allowed_hosts TEXT,
    secrets_config TEXT,

    -- Port forwarding (JSON array)
    port_mappings TEXT DEFAULT '[]',

    -- Host ports exposed to sandbox (JSON array)
    expose_host_ports TEXT DEFAULT '[]',

    -- Sandbox runtime
    sandbox_instance_dir TEXT,
    sandbox_checkpoint_name TEXT,

    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    stopped_at TEXT,
    deleting_at TEXT
);

CREATE TABLE IF NOT EXISTS snapshots (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id INTEGER NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    checkpoint_name TEXT NOT NULL,
    auto_generated INTEGER NOT NULL DEFAULT 0,
    trigger TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS agents (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    command TEXT NOT NULL,
    icon TEXT,
    color TEXT,
    is_builtin INTEGER NOT NULL DEFAULT 0,
    tab_order INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS terminal_tabs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id INTEGER NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    label TEXT NOT NULL DEFAULT 'Terminal',
    agent_id INTEGER REFERENCES agents(id),
    checkpoint_name TEXT,
    tab_order INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    last_active_workspace_id INTEGER REFERENCES workspaces(id) ON DELETE SET NULL,
    theme TEXT NOT NULL DEFAULT 'dark',
    terminal_font_family TEXT NOT NULL DEFAULT 'Berkeley Mono',
    terminal_font_size INTEGER NOT NULL DEFAULT 13,
    sidebar_width INTEGER NOT NULL DEFAULT 260,
    review_panel_width INTEGER NOT NULL DEFAULT 420,
    confirm_on_quit INTEGER NOT NULL DEFAULT 1,
    default_agent_id INTEGER REFERENCES agents(id) ON DELETE SET NULL,
    sandbox_cpus INTEGER NOT NULL DEFAULT 2,
    sandbox_memory_mb INTEGER NOT NULL DEFAULT 8192,
    sandbox_disk_mb INTEGER NOT NULL DEFAULT 16384,
    allowed_hosts TEXT
);

INSERT OR IGNORE INTO settings (id) VALUES (1);

CREATE TABLE IF NOT EXISTS secrets (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    env_var TEXT NOT NULL UNIQUE,
    label TEXT NOT NULL DEFAULT '',
    encrypted_value BLOB NOT NULL,
    hosts TEXT DEFAULT '[]',
    auth_method TEXT NOT NULL DEFAULT 'api_key',
    oauth_refresh_token BLOB,
    oauth_id_token BLOB,
    oauth_expires_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
