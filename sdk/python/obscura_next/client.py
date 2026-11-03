"""
Obscura Next Python SDK — Async and sync client for supervised worker IPC.
Communicates with the obscura-runtime-worker binary via framed JSON over stdin/stdout.
"""

import json
import asyncio
import os
import shutil
import time
import uuid
from typing import Optional, Dict, Any
from .models import SessionConfig, AgentAction, ActionResult, SemanticNode


def _resolve_worker_binary(binary_path: Optional[str] = None) -> str:
    """Resolves the obscura-runtime-worker binary in priority order."""
    # 1. Explicit argument
    if binary_path and os.path.isfile(binary_path) and os.access(binary_path, os.X_OK):
        return binary_path

    # 2. Environment variable
    env_bin = os.environ.get("OBSCURA_WORKER_BIN")
    if env_bin and os.path.isfile(env_bin) and os.access(env_bin, os.X_OK):
        return env_bin

    # 3. Build directories relative to current working directory
    candidates = [
        "target/debug/obscura-runtime-worker",
        "target/release/obscura-runtime-worker",
        "target/debug/obscura-worker",
        "target/release/obscura-worker",
        "../target/debug/obscura-runtime-worker",
        "../target/release/obscura-runtime-worker",
    ]
    for c in candidates:
        if os.path.isfile(c) and os.access(c, os.X_OK):
            return os.path.abspath(c)

    # 4. PATH lookup
    found = shutil.which("obscura-runtime-worker") or shutil.which("obscura-worker")
    if found:
        return found

    # 5. Fallback (will likely fail, but gives a clear error)
    return binary_path or "obscura-runtime-worker"


class AsyncObscuraClient:
    """Async client communicating with Obscura Next isolated runtime worker.

    Uses the canonical framed JSON protocol (one JSON object per line over stdin/stdout).
    Every request carries an `id` for correlation and a `session_id` for session-scoped commands.
    """

    def __init__(self, binary_path: Optional[str] = None, default_session_id: Optional[str] = None):
        self.binary_path = _resolve_worker_binary(binary_path)
        self.default_session_id = default_session_id or f"py-session-{uuid.uuid4().hex[:12]}"
        self._proc: Optional[asyncio.subprocess.Process] = None
        self._req_counter = 0

    def _next_id(self) -> str:
        self._req_counter += 1
        return f"py-{self._req_counter}"

    async def start(self):
        """Starts the worker process with framed IPC (no subcommand)."""
        self._proc = await asyncio.create_subprocess_exec(
            self.binary_path,
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )

    async def send_command(self, cmd: Dict[str, Any]) -> Dict[str, Any]:
        """Sends a canonical protocol command and returns the parsed response."""
        if not self._proc or not self._proc.stdin or not self._proc.stdout:
            raise RuntimeError("Client not connected to worker process")

        if "id" not in cmd:
            cmd["id"] = self._next_id()

        line = json.dumps(cmd) + "\n"
        self._proc.stdin.write(line.encode("utf-8"))
        await self._proc.stdin.drain()

        resp_line = await self._proc.stdout.readline()
        if not resp_line:
            raise RuntimeError("Worker process terminated unexpectedly")

        return json.loads(resp_line.decode("utf-8"))

    async def ping(self) -> Dict[str, Any]:
        """Sends a ping command to the worker."""
        return await self.send_command({"cmd": "ping"})

    async def health(self) -> Dict[str, Any]:
        """Sends a health check command to the worker."""
        return await self.send_command({"cmd": "health"})

    async def create_session(self, session_id: Optional[str] = None, config: Optional[SessionConfig] = None) -> Dict[str, Any]:
        """Creates a new browser session."""
        sid = session_id or self.default_session_id
        cmd: Dict[str, Any] = {
            "cmd": "session_create",
            "session_id": sid,
        }
        if config:
            cmd["config"] = {
                "stealth": config.stealth,
                "timeout_ms": config.timeout_ms,
            }
            if config.proxy_url:
                cmd["config"]["proxy_url"] = config.proxy_url
            if config.user_agent:
                cmd["config"]["user_agent"] = config.user_agent
        return await self.send_command(cmd)

    async def destroy_session(self, session_id: Optional[str] = None) -> Dict[str, Any]:
        """Destroys a browser session."""
        sid = session_id or self.default_session_id
        return await self.send_command({
            "cmd": "session_destroy",
            "session_id": sid,
        })

    async def navigate(self, url: str, session_id: Optional[str] = None, wait_until: str = "load", timeout_ms: Optional[int] = None) -> ActionResult:
        """Navigates the session's page to a URL."""
        sid = session_id or self.default_session_id
        t0 = time.time()
        cmd: Dict[str, Any] = {
            "cmd": "navigate",
            "session_id": sid,
            "url": url,
            "waitUntil": wait_until,
        }
        if timeout_ms is not None:
            cmd["timeout_ms"] = timeout_ms
        resp = await self.send_command(cmd)
        elapsed = (time.time() - t0) * 1000.0
        return ActionResult(
            action="navigate",
            success=resp.get("ok", False),
            payload=resp.get("result"),
            elapsed_ms=elapsed,
            error=resp.get("error"),
        )

    async def evaluate(self, expression: str, session_id: Optional[str] = None, await_promise: bool = True, timeout_ms: Optional[int] = None) -> ActionResult:
        """Evaluates a JavaScript expression in the session."""
        sid = session_id or self.default_session_id
        t0 = time.time()
        cmd: Dict[str, Any] = {
            "cmd": "evaluate",
            "session_id": sid,
            "expression": expression,
            "await_promise": await_promise,
        }
        if timeout_ms is not None:
            cmd["timeout_ms"] = timeout_ms
        resp = await self.send_command(cmd)
        elapsed = (time.time() - t0) * 1000.0
        return ActionResult(
            action="evaluate",
            success=resp.get("ok", False),
            payload=resp.get("result"),
            elapsed_ms=elapsed,
            error=resp.get("error"),
        )

    async def snapshot(self, session_id: Optional[str] = None) -> Dict[str, Any]:
        """Gets the accessibility snapshot of the current page."""
        sid = session_id or self.default_session_id
        return await self.send_command({
            "cmd": "snapshot",
            "session_id": sid,
        })

    async def click(self, selector: str, session_id: Optional[str] = None) -> ActionResult:
        """Clicks an element matching the CSS selector."""
        sid = session_id or self.default_session_id
        t0 = time.time()
        resp = await self.send_command({
            "cmd": "click",
            "session_id": sid,
            "selector": selector,
        })
        elapsed = (time.time() - t0) * 1000.0
        return ActionResult(action="click", success=resp.get("ok", False), payload=resp.get("result"), elapsed_ms=elapsed, error=resp.get("error"))

    async def fill(self, selector: str, value: str, session_id: Optional[str] = None) -> ActionResult:
        """Fills an input element matching the CSS selector with the given value."""
        sid = session_id or self.default_session_id
        t0 = time.time()
        resp = await self.send_command({
            "cmd": "fill",
            "session_id": sid,
            "selector": selector,
            "value": value,
        })
        elapsed = (time.time() - t0) * 1000.0
        return ActionResult(action="fill", success=resp.get("ok", False), payload=resp.get("result"), elapsed_ms=elapsed, error=resp.get("error"))

    async def type_text(self, selector: str, text: str, session_id: Optional[str] = None) -> ActionResult:
        """Types text into an element matching the CSS selector."""
        sid = session_id or self.default_session_id
        t0 = time.time()
        resp = await self.send_command({
            "cmd": "type",
            "session_id": sid,
            "selector": selector,
            "text": text,
        })
        elapsed = (time.time() - t0) * 1000.0
        return ActionResult(action="type", success=resp.get("ok", False), payload=resp.get("result"), elapsed_ms=elapsed, error=resp.get("error"))

    async def screenshot(self, session_id: Optional[str] = None) -> Dict[str, Any]:
        """Takes a screenshot of the current page (returns base64 data)."""
        sid = session_id or self.default_session_id
        return await self.send_command({
            "cmd": "screenshot",
            "session_id": sid,
        })

    async def title(self, session_id: Optional[str] = None) -> str:
        """Gets the page title."""
        sid = session_id or self.default_session_id
        resp = await self.send_command({
            "cmd": "title",
            "session_id": sid,
        })
        if resp.get("ok") and resp.get("result") is not None:
            r = resp["result"]
            if isinstance(r, str):
                return r
            if isinstance(r, dict):
                return r.get("title", "")
        return ""

    async def dump(self, format: str = "html", session_id: Optional[str] = None) -> str:
        """Dumps the page content (html or text)."""
        sid = session_id or self.default_session_id
        resp = await self.send_command({
            "cmd": "dump",
            "session_id": sid,
            "format": format,
        })
        if resp.get("ok") and resp.get("result") is not None:
            r = resp["result"]
            if isinstance(r, str):
                return r
            if isinstance(r, dict):
                return r.get("content", "")
        return ""

    async def close(self):
        """Sends shutdown and cleans up the worker process."""
        if self._proc:
            try:
                await self.send_command({"cmd": "shutdown"})
            except Exception:
                pass
            if self._proc.stdin:
                self._proc.stdin.close()
            await self._proc.wait()
            self._proc = None


class ObscuraClient:
    """Synchronous convenience wrapper around AsyncObscuraClient.

    Usage:
        with ObscuraClient() as client:
            client.create_session()
            result = client.navigate("https://example.com")
            print(result)
    """

    def __init__(self, binary_path: Optional[str] = None, session_id: Optional[str] = None):
        self._async_client = AsyncObscuraClient(binary_path, session_id)

    def __enter__(self):
        asyncio.run(self._async_client.start())
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        asyncio.run(self._async_client.close())

    def ping(self) -> Dict[str, Any]:
        return asyncio.run(self._async_client.ping())

    def health(self) -> Dict[str, Any]:
        return asyncio.run(self._async_client.health())

    def create_session(self, session_id: Optional[str] = None, config: Optional[SessionConfig] = None) -> Dict[str, Any]:
        return asyncio.run(self._async_client.create_session(session_id, config))

    def destroy_session(self, session_id: Optional[str] = None) -> Dict[str, Any]:
        return asyncio.run(self._async_client.destroy_session(session_id))

    def navigate(self, url: str, session_id: Optional[str] = None, wait_until: str = "load") -> ActionResult:
        return asyncio.run(self._async_client.navigate(url, session_id=session_id, wait_until=wait_until))

    def evaluate(self, expression: str, session_id: Optional[str] = None) -> ActionResult:
        return asyncio.run(self._async_client.evaluate(expression, session_id=session_id))

    def snapshot(self, session_id: Optional[str] = None) -> Dict[str, Any]:
        return asyncio.run(self._async_client.snapshot(session_id))

    def click(self, selector: str, session_id: Optional[str] = None) -> ActionResult:
        return asyncio.run(self._async_client.click(selector, session_id))

    def fill(self, selector: str, value: str, session_id: Optional[str] = None) -> ActionResult:
        return asyncio.run(self._async_client.fill(selector, value, session_id))

    def type_text(self, selector: str, text: str, session_id: Optional[str] = None) -> ActionResult:
        return asyncio.run(self._async_client.type_text(selector, text, session_id))

    def screenshot(self, session_id: Optional[str] = None) -> Dict[str, Any]:
        return asyncio.run(self._async_client.screenshot(session_id))

    def title(self, session_id: Optional[str] = None) -> str:
        return asyncio.run(self._async_client.title(session_id))

    def dump(self, format: str = "html", session_id: Optional[str] = None) -> str:
        return asyncio.run(self._async_client.dump(format, session_id))
