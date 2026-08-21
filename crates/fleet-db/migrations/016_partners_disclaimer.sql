-- Partners disclaimer ack is a recorded timestamp only.
-- NULL = not accepted (the default). Setting it does not enable
-- lending, borrowing, enqueue, or assignable partner runners.

ALTER TABLE workspaces
    ADD COLUMN partners_disclaimer_accepted_at TIMESTAMPTZ;
