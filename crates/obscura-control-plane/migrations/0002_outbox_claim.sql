ALTER TABLE outbox_events ADD COLUMN IF NOT EXISTS claimed_at TIMESTAMPTZ;
ALTER TABLE outbox_events ADD COLUMN IF NOT EXISTS claimed_by TEXT;
CREATE INDEX IF NOT EXISTS idx_outbox_claimed ON outbox_events (claimed_at);
