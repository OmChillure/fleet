-- Driver-produced live stream. Plane is the log the UI reads.
-- Host live.jsonl is a local debug file only.
CREATE TABLE worker_live_events (
    session_id  UUID NOT NULL REFERENCES worker_sessions(id) ON DELETE CASCADE,
    report_seq  INT NOT NULL,
    idx         INT NOT NULL,
    ts          TIMESTAMPTZ NOT NULL,
    kind        TEXT NOT NULL,
    body        TEXT NOT NULL,
    PRIMARY KEY (session_id, report_seq, idx)
);
CREATE INDEX worker_live_events_session_idx
    ON worker_live_events (session_id, report_seq, idx);
