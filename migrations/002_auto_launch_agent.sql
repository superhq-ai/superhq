-- Add auto_launch_agent setting
ALTER TABLE settings ADD COLUMN auto_launch_agent INTEGER NOT NULL DEFAULT 1;
