-- Generalized inbox: listen on Slack / Notion (and still ticket boards).
-- auto_launch starts a fleet for new chat/page items (not every old Jira issue).

ALTER TABLE ticket_monitors
    ADD COLUMN IF NOT EXISTS slack_channels TEXT[] NOT NULL DEFAULT '{}',
    ADD COLUMN IF NOT EXISTS notion_databases TEXT[] NOT NULL DEFAULT '{}',
    ADD COLUMN IF NOT EXISTS auto_launch BOOLEAN NOT NULL DEFAULT false;
