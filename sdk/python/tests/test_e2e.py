"""End-to-end integration tests for the Python SDK against the real worker binary."""

import asyncio
import os
import sys
import pytest
import pytest_asyncio

# Add SDK to path
sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from obscura_next.client import AsyncObscuraClient, _resolve_worker_binary


def _find_binary():
    """Resolve the worker binary relative to the project root."""
    project_root = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
    candidates = [
        os.path.join(project_root, "target", "debug", "obscura-runtime-worker"),
        os.path.join(project_root, "target", "release", "obscura-runtime-worker"),
    ]
    for c in candidates:
        if os.path.isfile(c) and os.access(c, os.X_OK):
            return c
    return None


WORKER_BIN = _find_binary()
SKIP_REASON = "Worker binary not found; build with `cargo build -p obscura-worker` first"


@pytest.mark.skipif(WORKER_BIN is None, reason=SKIP_REASON)
class TestAsyncObscuraClient:
    @pytest_asyncio.fixture
    async def client(self):
        c = AsyncObscuraClient(binary_path=WORKER_BIN)
        await c.start()
        yield c
        await c.close()

    @pytest.mark.asyncio
    async def test_ping(self, client):
        resp = await client.ping()
        assert resp["ok"] is True

    @pytest.mark.asyncio
    async def test_health(self, client):
        resp = await client.health()
        assert resp["ok"] is True

    @pytest.mark.asyncio
    async def test_session_lifecycle(self, client):
        sid = "test-py-session-1"
        # Create
        resp = await client.create_session(session_id=sid)
        assert resp["ok"] is True, f"Create failed: {resp}"

        # Navigate
        result = await client.navigate("data:text/html,<h1>Hello Python SDK</h1>", session_id=sid)
        assert result.success, f"Navigate failed: {result.error}"

        # Evaluate
        result = await client.evaluate("1 + 2", session_id=sid)
        assert result.success, f"Evaluate failed: {result.error}"

        # Destroy
        resp = await client.destroy_session(session_id=sid)
        assert resp["ok"] is True, f"Destroy failed: {resp}"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])

