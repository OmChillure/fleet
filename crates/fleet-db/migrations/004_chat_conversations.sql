-- Workspace chat history (UI Sessions + resume in Chat).
-- Conversations are per-user within a workspace; messages are append-only.

CREATE TABLE chat_conversations (
    id              UUID PRIMARY KEY,
    workspace_id    UUID NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    user_id         UUID NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    title           TEXT NOT NULL DEFAULT 'New chat',
    -- Last provider/model used (for display / resume defaults)
    provider        TEXT,
    model           TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX chat_conversations_workspace_user_updated_idx
    ON chat_conversations (workspace_id, user_id, updated_at DESC);

CREATE TABLE chat_messages (
    id                UUID PRIMARY KEY,
    conversation_id   UUID NOT NULL REFERENCES chat_conversations (id) ON DELETE CASCADE,
    -- user | assistant | system | tool
    role              TEXT NOT NULL
                      CHECK (role IN ('user', 'assistant', 'system', 'tool')),
    content           TEXT NOT NULL DEFAULT '',
    provider          TEXT,
    model             TEXT,
    -- Tool/sandbox actions attached to assistant turns (JSON array)
    actions           JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX chat_messages_conversation_created_idx
    ON chat_messages (conversation_id, created_at ASC);
