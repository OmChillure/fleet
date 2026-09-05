-- Opaque token so providers can POST inbox events without a user cookie.
-- URL: POST /api/integrations/webhooks/{provider}/{hook_token}

ALTER TABLE ticket_monitors
    ADD COLUMN IF NOT EXISTS hook_token TEXT;

UPDATE ticket_monitors
SET hook_token = replace(gen_random_uuid()::text || gen_random_uuid()::text, '-', '')
WHERE hook_token IS NULL OR hook_token = '';

ALTER TABLE ticket_monitors
    ALTER COLUMN hook_token SET NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS ticket_monitors_hook_token_uidx
    ON ticket_monitors (hook_token);
