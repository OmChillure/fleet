-- Durable control-plane event outbox (BusMessage bytes + publish watermark).
-- publisher polls unpublished rows and marks them after a successful put.

CREATE TABLE control_plane_events (
    id            UUID PRIMARY KEY,
    message_id    UUID NOT NULL UNIQUE,
    "type"        TEXT NOT NULL,
    body          JSONB NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    published_at  TIMESTAMPTZ
);

CREATE INDEX control_plane_events_unpublished_idx
    ON control_plane_events (published_at)
    WHERE published_at IS NULL;
