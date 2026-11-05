CREATE TABLE IF NOT EXISTS credentials (
    credential_id TEXT PRIMARY KEY,
    subject TEXT NOT NULL,
    tenant_id UUID REFERENCES tenants(tenant_id),
    secret_hash TEXT NOT NULL,
    is_admin BOOLEAN NOT NULL DEFAULT false,
    is_worker BOOLEAN NOT NULL DEFAULT false,
    revoked BOOLEAN NOT NULL DEFAULT false,
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_credentials_tenant ON credentials (tenant_id);
