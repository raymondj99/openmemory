"""Minimal JSON-RPC 2.0 stdio client for the openmemory MCP server."""

import json
import os
import subprocess
import time

BINARY = os.environ.get(
    "OPENMEMORY_BIN",
    os.path.join(os.path.dirname(__file__), "../../../target/release/openmemory"),
)
HOME = os.environ.get(
    "TRILAYER_HOME",
    os.path.abspath(os.path.join(os.path.dirname(__file__), "../.home")),
)


class McpClient:
    def __init__(self):
        env = dict(os.environ, OPENMEMORY_HOME=HOME)
        self.proc = subprocess.Popen(
            [BINARY, "mcp"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            env=env,
            text=True,
            bufsize=1,
        )
        self._id = 0
        self._request(
            "initialize",
            {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "trilayer-harness", "version": "0.1"},
            },
        )
        self._notify("notifications/initialized")

    def _notify(self, method):
        self.proc.stdin.write(json.dumps({"jsonrpc": "2.0", "method": method}) + "\n")
        self.proc.stdin.flush()

    def _request(self, method, params):
        self._id += 1
        msg = {"jsonrpc": "2.0", "id": self._id, "method": method, "params": params}
        self.proc.stdin.write(json.dumps(msg) + "\n")
        self.proc.stdin.flush()
        while True:
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError("server closed stdout")
            resp = json.loads(line)
            if resp.get("id") == self._id:
                if "error" in resp:
                    raise RuntimeError(f"{method}: {resp['error']}")
                return resp["result"]

    def call(self, tool, arguments):
        """Call a tool; returns (parsed_payload, elapsed_seconds)."""
        t0 = time.monotonic()
        result = self._request("tools/call", {"name": tool, "arguments": arguments})
        elapsed = time.monotonic() - t0
        text = ""
        for item in result.get("content", []):
            if item.get("type") == "text":
                text += item["text"]
        try:
            payload = json.loads(text)
        except (json.JSONDecodeError, ValueError):
            payload = text
        if result.get("isError"):
            raise RuntimeError(f"{tool} tool error: {text[:400]}")
        return payload, elapsed

    def close(self):
        try:
            self.proc.stdin.close()
            self.proc.wait(timeout=15)
        except Exception:
            self.proc.kill()
