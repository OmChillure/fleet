-- Workspace Settings: concurrent agents already live on project_configs.max_agents.
-- Fleet worker LLM (local Ollama). Grok stays env-only (FLEET_AGENT_BACKEND=grok).

ALTER TABLE project_configs
    ADD COLUMN IF NOT EXISTS fleet_llm TEXT NOT NULL DEFAULT 'off',
    ADD COLUMN IF NOT EXISTS fleet_llm_model TEXT NOT NULL DEFAULT '';

ALTER TABLE project_configs
    DROP CONSTRAINT IF EXISTS project_configs_fleet_llm_check;

ALTER TABLE project_configs
    ADD CONSTRAINT project_configs_fleet_llm_check
    CHECK (fleet_llm IN ('off', 'ollama'));
