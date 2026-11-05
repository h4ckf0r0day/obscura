# Credential Identity Post-Commit Security Audit

## Audited Commit
a0ed4c40e90945cbb29ef13d8ad6ffe51cd0244f

## Branch
obscura-next/v0.3.2-credential-backed-identity

## Working Tree
Clean.

---

## WP1 — Service Principal Security

### Findings

1. **`service_principal()` is public and mints arbitrary privileged principals.**
   - Location: `crates/obscura-control-plane/src/lib.rs:978-981`
   - Any caller with a `ControlPlane` instance can call `cp.service_principal("admin")` and receive a principal with `is_admin=true`, `tenant_id=None`.
   - This is a **security bypass**: untrusted callers can obtain administrative privileges.
   - Severity: P1

2. **No binding to registered worker identity.**
   - `Principal::worker()` exists but is `pub(crate)`. `service_principal("worker-agent")` is used instead, granting admin=true.
   - Worker identity is not verified against the workers table.
   - Severity: P1

3. **Internal methods accessible via service_principal("internal").**
   - Any caller can obtain an internal service principal and invoke `*_internal` methods.
   - No credential verification gates this.
   - Severity: P1

### Caller/Privilege Matrix

| Caller | Method Used | Principal Type | Privileges | Can Forge? |
|---|---|---|---|---|
| Scheduler | service_principal("scheduler") | Service (admin) | Admin | Yes (any ControlPlane holder) |
| Worker agent | service_principal("worker-agent") | Service (admin) | Admin | Yes |
| Internal methods | service_principal("internal") | Service (admin) | Admin | Yes |
| Tests | service_principal (via internal) | Service (admin) | Admin | Yes |
| External (none yet) | N/A | N/A | N/A | N/A |

### Assessment: NOT VERIFIED
Service principals are mintable by any code with ControlPlane access. No credential verification gates service identity.

---

## WP2 — Credential Verification

### Findings

1. **Secret comparison is NOT constant-time.**
   - Location: `crates/obscura-control-plane/src/lib.rs:1028`
   - Uses `stored_hash != input_hash` (string inequality), which is not constant-time.
   - Severity: P2

2. **SHA-256 is used for credential hashing.**
   - Acceptable for high-entropy API tokens but not ideal for low-entropy secrets.
   - No salt or pepper.
   - Severity: P3 (acceptable for API tokens, document limitation)

3. **Error messages do not leak existence.**
   - "invalid credential" returned for both missing and wrong secret. ✓ Good.

4. **Raw credentials never logged.**
   - No logging of secrets in authenticate or create_credential. ✓ Good.

5. **Revocation and expiry checked.**
   - Both checked on every authenticate call. ✓ Good.

6. **Tenant association from trusted record.**
   - tenant_id comes from DB row, not caller input. ✓ Good.

7. **Admin/worker flags from trusted record.**
   - is_admin/is_worker come from DB row, not caller input. ✓ Good.

### Assessment: PARTIALLY VERIFIED
Credential verification works functionally but uses non-constant-time comparison and lacks salt.

---

## WP3 — Credential Lifecycle Authorization

### Findings

1. **`create_credential` has no authorization check.**
   - Any caller with ControlPlane access can create credentials for any tenant.
   - Severity: P1

2. **`revoke_credential` has no authorization check.**
   - Any caller can revoke any credential.
   - Severity: P1

3. **No bootstrap procedure documented.**
   - No safe initial admin credential creation process.
   - Severity: P2

4. **No audit logging for credential operations.**
   - audit_events table exists but is not populated.
   - Severity: P3

### Assessment: NOT VERIFIED
Credential management has no authorization. Any caller can create/revoke credentials.

---

## WP4 — Principal and Authorization Integrity

### Findings

1. **Principal fields are private (good).**
   - External crates cannot construct Principal directly. ✓

2. **But `service_principal()` is public and mints admin principals.**
   - Any caller with ControlPlane can mint arbitrary service principals.
   - This undermines the private-field protection.
   - Severity: P1

3. **All protected methods require `&Principal`.**
   - Signatures enforce presence. ✓ (improvement from ba6bf51)

4. **`authorize_tenant` is called in all protected methods.**
   - But it only checks `can_access_tenant`, which allows admin (tenant_id=None) to access any tenant.
   - A service principal with is_admin=true bypasses tenant isolation.
   - Severity: P1

5. **No permission granularity.**
   - Only is_admin/is_worker booleans. No fine-grained permissions.
   - Severity: P3

### Assessment: PARTIALLY VERIFIED
Principal is unforgeable externally, but service_principal() is a backdoor to admin privileges.

---

## WP5 — Network Boundary Assessment

### Findings

1. **No HTTP/network API exists.**
   - Control plane is a library. No network-facing authentication boundary.
   - Cannot claim production API authentication.
   - Severity: P2 (architectural gap, not code defect)

2. **No TLS or transport security.**
   - Not implemented.
   - Severity: P2

### Assessment: NOT VERIFIED (no network boundary to test)

---

## WP6 — Tests

### Available Tests
- `auth_tests::test_credential_auth` (control-plane crate): tests valid/invalid/revoked/expired credentials.

### Missing Tests
- Principal forgery (cannot construct externally — compile-time, not runtime test).
- Service principal escalation.
- Worker impersonation.
- Scheduler impersonation.
- Cross-tenant access.
- Credential creation authorization.
- Credential revocation authorization.
- Credential rotation.

### Test Results
- `cargo check --workspace`: PASS
- `cargo test -p obscura-control-plane`: Could not run (Docker daemon down during this audit).
- Auth tests: BLOCKED (no PostgreSQL available).

---

## F2 Closure Assessment

F2 (Authentication) can be CLOSED only when:
- Principal construction is controlled. ✓ (fields private, constructors pub(crate))
- Credentials are verified at trust boundary. ✗ (no network boundary; service_principal is unguarded)
- Revoked/expired credentials rejected. ✓ (authenticate checks)
- Service identities authenticated. ✗ (service_principal not credential-backed)

**F2: NOT CLOSED — PARTIALLY RESOLVED**

## F3 Closure Assessment

F3 (Authorization) can be CLOSED only when:
- Permissions come from trusted identity records. ✗ (service principals are self-minted)
- Every protected operation checks authorization. ✓ (authorize_tenant called)
- Cross-tenant access tests pass. ✗ (no tests)
- Internal service permissions restricted. ✗ (service_principal grants admin to anyone)
- No public method permits caller-controlled privilege escalation. ✗ (service_principal allows arbitrary name)

**F3: NOT CLOSED — PARTIALLY RESOLVED**

---

## Exact Vulnerabilities

1. **V1 (P1):** `service_principal()` is public and mints admin principals for any caller.
2. **V2 (P1):** `create_credential` and `revoke_credential` have no authorization checks.
3. **V3 (P2):** Secret comparison uses `!=` (not constant-time).
4. **V4 (P2):** No network-facing authentication boundary exists.
5. **V5 (P3):** No credential rotation support.
6. **V6 (P3):** No audit logging for credential operations.

## Recommended Corrective Actions

1. **Immediate:** Make `service_principal` require a verified service credential (not just a name string).
2. **Immediate:** Add authorization checks to `create_credential` and `revoke_credential`.
3. **High:** Replace `!=` with constant-time comparison (use `subtle` crate or `constant_time_eq`).
4. **High:** Implement credential rotation.
5. **Medium:** Add audit logging for credential operations.
6. **Medium:** Design network-facing auth boundary when HTTP API is added.

## Final Verdict

**NOT READY.** Principal construction is controlled at the type level, but service identity is not credential-backed. `service_principal()` is a public backdoor to administrative privileges. Credential management has no authorization. F2 and F3 remain PARTIALLY RESOLVED.
