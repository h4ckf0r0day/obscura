import json
import asyncio
import subprocess
import time
from typing import Optional, Dict, Any, List
from .models import SessionConfig, AgentAction, ActionResult, SemanticNode

class AsyncObscuraClient:
    """Async client communicating with Obscura Next isolated runtime worker."""

    def __init__(self, binary_path: str = "obscura"):
        self.binary_path = binary_path
        self._proc: Optional[asyncio.subprocess.Process] = None

    async def start(self):
        """Starts the worker process with framed IPC."""
        self._proc = await asyncio.create_subprocess_exec(
            self.binary_path,
            "worker",
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )

    async def send_command(self, cmd: Dict[str, Any]) -> Dict[str, Any]:
        if not self._proc or not self._proc.stdin or not self._proc.stdout:
            raise RuntimeError("Client not connected to worker process")

        line = json.dumps(cmd) + "\n"
        self._proc.stdin.write(line.encode("utf-8"))
        await self._proc.stdin.drain()

        resp_line = await self._proc.stdout.readline()
        if not resp_line:
            raise RuntimeError("Worker process terminated unexpectedly")

        return json.loads(resp_line.decode("utf-8"))

    async def navigate(self, url: str, wait_until: str = "load") -> ActionResult:
        t0 = time.time()
        cmd = {"cmd": "navigate", "url": url, "waitUntil": wait_until}
        resp = await self.send_command(cmd)
        elapsed = (time.time() - t0) * 1000.0
        return ActionResult(
            action="navigate",
            success=resp.get("ok", False),
            payload=resp.get("result"),
            elapsed_ms=elapsed,
            error=resp.get("error"),
        )

    async def evaluate(self, expression: str) -> ActionResult:
        t0 = time.time()
        cmd = {"cmd": "evaluate", "expression": expression}
        resp = await self.send_command(cmd)
        elapsed = (time.time() - t0) * 1000.0
        return ActionResult(
            action="evaluate",
            success=resp.get("ok", False),
            payload=resp.get("result"),
            elapsed_ms=elapsed,
            error=resp.get("error"),
        )

    async def dump(self, format: str = "html") -> str:
        cmd_name = "dump_html" if format == "html" else "dump_text"
        resp = await self.send_command({"cmd": cmd_name})
        return resp.get("result", "")

    async def close(self):
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
    """Synchronous convenience wrapper around AsyncObscuraClient."""

    def __init__(self, binary_path: str = "obscura"):
        self._async_client = AsyncObscuraClient(binary_path)

    def __enter__(self):
        asyncio.run(self._async_client.start())
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        asyncio.run(self._async_client.close())

    def navigate(self, url: str, wait_until: str = "load") -> ActionResult:
        return asyncio.run(self._async_client.navigate(url, wait_until))

    def evaluate(self, expression: str) -> ActionResult:
        return asyncio.run(self._async_client.evaluate(expression))

    def dump(self, format: str = "html") -> str:
        return asyncio.run(self._async_client.dump(format))
