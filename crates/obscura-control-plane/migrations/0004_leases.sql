CREATE TABLE IF NOT EXISTS leases (
    lease_id UUID PRIMARY KEY,
    resource_type TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    owner_id UUID NOT NULL,
    fencing_token BIGINT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    renewed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (resource_type, resource_id)
);
CREATE SEQUENCE IF NOT EXISTS fencing_token_seq START 1;
