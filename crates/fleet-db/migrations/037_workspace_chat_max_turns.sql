-- How many official-CLI agent steps one Chat message may take on the runner.
-- 1 is "reply only"; a first-turn tool call then exits "Max turns reached".
ALTER TABLE workspaces
    ADD COLUMN chat_max_turns INT NOT NULL DEFAULT 32;

ALTER TABLE workspaces
    ADD CONSTRAINT workspaces_chat_max_turns_check
    CHECK (chat_max_turns >= 1 AND chat_max_turns <= 128);
