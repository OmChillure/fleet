-- One git remote per project (multi-repo later). Used by the driver to clone
-- the workspace before an agent turn. NULL repo_url = no clone (tests / mock).

ALTER TABLE project_configs
    ADD COLUMN repo_url TEXT,
    ADD COLUMN git_ref TEXT NOT NULL DEFAULT 'main';
