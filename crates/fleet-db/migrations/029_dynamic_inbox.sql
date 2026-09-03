-- Dynamic inbox: follow whatever is connected, generic per-provider filters, caps.
-- Legacy github_repos / jira_jql / slack_channels / notion_databases stay dual-written.

ALTER TABLE ticket_monitors
    ADD COLUMN IF NOT EXISTS follow_connected BOOLEAN NOT NULL DEFAULT true,
    ADD COLUMN IF NOT EXISTS filters JSONB NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS launch_limit INT NOT NULL DEFAULT 8
        CHECK (launch_limit >= 1 AND launch_limit <= 50),
    ADD COLUMN IF NOT EXISTS poll_limit INT NOT NULL DEFAULT 20
        CHECK (poll_limit >= 1 AND poll_limit <= 100);

UPDATE ticket_monitors
SET filters = jsonb_strip_nulls(jsonb_build_object(
        'github', CASE
            WHEN cardinality(github_repos) > 0
                THEN jsonb_build_object('repos', to_jsonb(github_repos))
        END,
        'jira', CASE
            WHEN jira_jql IS NOT NULL AND length(trim(jira_jql)) > 0
                THEN jsonb_build_object('jql', jira_jql)
        END,
        'slack', CASE
            WHEN cardinality(slack_channels) > 0
                THEN jsonb_build_object('channels', to_jsonb(slack_channels))
        END,
        'notion', CASE
            WHEN cardinality(notion_databases) > 0
                THEN jsonb_build_object('databases', to_jsonb(notion_databases))
        END
    )),
    -- Empty allowlist + follow_connected means "whatever is connected now".
    -- Existing snapshotted provider lists were an implicit "all at click time".
    providers = '{}',
    follow_connected = true
WHERE follow_connected = true;
