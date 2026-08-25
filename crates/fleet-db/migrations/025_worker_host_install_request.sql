-- Queue one model install for the next heartbeat (`ollama pull …`).
ALTER TABLE worker_hosts
    ADD COLUMN IF NOT EXISTS install_request jsonb,
    ADD COLUMN IF NOT EXISTS install_requested_at timestamptz;
