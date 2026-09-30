-- Several named connections per provider, and one category list for Inbox
-- and Integrations.

ALTER TABLE integration_connections
    ADD COLUMN display_name TEXT NOT NULL DEFAULT '';

UPDATE integration_connections
SET display_name = COALESCE(NULLIF(btrim(account_label), ''), provider)
WHERE display_name = '';

DROP INDEX IF EXISTS integration_connections_workspace_provider_uidx;

-- Legacy reconnects still replace the single unnamed row.
CREATE UNIQUE INDEX integration_connections_unnamed_uidx
    ON integration_connections (workspace_id, provider)
    WHERE display_name = '' AND status <> 'disconnected';

-- "Work" and "Personal" are two Slack rows. A disconnected row can reuse a name.
CREATE UNIQUE INDEX integration_connections_named_uidx
    ON integration_connections (workspace_id, provider, display_name)
    WHERE display_name <> '' AND status <> 'disconnected';

CREATE TABLE integration_categories (
    id          TEXT PRIMARY KEY,
    label       TEXT NOT NULL,
    subtitle    TEXT NOT NULL,
    sort_order  INT NOT NULL
);

CREATE TABLE integration_provider_categories (
    provider     TEXT PRIMARY KEY,
    category_id  TEXT NOT NULL REFERENCES integration_categories (id)
);

INSERT INTO integration_categories (id, label, subtitle, sort_order) VALUES
    (
        'source_control',
        'Source control',
        'Open issues become Fleet runs. Agents implement them and open a PR or MR.',
        1
    ),
    (
        'issue_trackers',
        'Issue trackers',
        'Tickets and cards become a work + review run. Status stays on the board.',
        2
    ),
    (
        'chat',
        'Chat',
        'One message becomes one ops job. Fleet replies in the channel when it finishes.',
        3
    ),
    (
        'docs',
        'Docs',
        'A shared page becomes an analysis run — read-through, not a code change.',
        4
    ),
    (
        'design',
        'Design',
        'Design files agents can read while implementing UI.',
        5
    );

INSERT INTO integration_provider_categories (provider, category_id) VALUES
    ('github', 'source_control'),
    ('gitlab', 'source_control'),
    ('gitea', 'source_control'),
    ('bitbucket', 'source_control'),
    ('jira', 'issue_trackers'),
    ('linear', 'issue_trackers'),
    ('asana', 'issue_trackers'),
    ('trello', 'issue_trackers'),
    ('slack', 'chat'),
    ('teams', 'chat'),
    ('discord', 'chat'),
    ('notion', 'docs'),
    ('confluence', 'docs'),
    ('google_drive', 'docs'),
    ('figma', 'design');
