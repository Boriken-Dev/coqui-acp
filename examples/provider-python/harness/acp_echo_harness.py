"""An ACP agent that answers each prompt with its text, as an `acp_harness` library written in Python.

The module is the library: its id is its name, its version `__version__`, its providers the classes in
`__all__`. It speaks ACP itself, one JSON-RPC message per channel message, and keeps every rule of the
channel: `send` waits for room in a queue of a few messages and `close` wakes it; one thread of its own
answers, in order, and is the only caller of the sink; a sink that refuses ends the connection. A prompt
of `close` is answered, then the provider ends the connection itself.
"""

from __future__ import annotations

import json
import threading
from collections import deque
from typing import Any, Deque, Dict, List, Optional, Tuple

from coqui_acp_harness_kinds import AcpHarness, Channel, ChannelSink

from guatiao import Ref, provide
from guatiao.errors import Gone, Unsupported

__version__ = "1.0.0"
__all__ = ["EchoHarness"]

#: How many messages wait for the sink before `send` waits too.
QUEUE = 4

#: The requirements an echo holds to: it writes nothing and runs nothing.
HOLDS = ("read_only", "no_commands")

#: A Python exception with no status of its own ends the stream as GUATIAO_ERR_INTERNAL.
INTERNAL = 100


class _Queue:
    """What the connection and its thread share."""

    def __init__(self) -> None:
        self.cond = threading.Condition()
        self.items: Deque[bytes] = deque()
        #: Either side has ended the connection: `send` answers GONE.
        self.closed = False
        #: The status the host closed with, when it was the host.
        self.host_status: Optional[int] = None

    def end(self) -> None:
        """Ends the connection from the provider's side, dropping what is still queued."""
        with self.cond:
            self.closed = True
            self.items.clear()
            self.cond.notify_all()


class Connection(Channel):
    """The host's end. The thread holds the queue, not this object, so dropping it closes."""

    def __init__(self, queue: _Queue) -> None:
        self._queue = queue

    def send(self, bytes: bytes) -> None:
        q = self._queue
        with q.cond:
            while True:
                if q.closed:
                    raise Gone("the connection is closed")
                if len(q.items) < QUEUE:
                    break
                q.cond.wait()
            q.items.append(bytes)
            q.cond.notify_all()

    def close(self, status: int) -> None:
        q = self._queue
        with q.cond:
            if q.closed:
                return
            q.closed = True
            q.host_status = status
            q.cond.notify_all()

    def __del__(self) -> None:
        self.close(0)


def _result(id: Any, result: Dict[str, Any]) -> Dict[str, Any]:
    return {"jsonrpc": "2.0", "id": id, "result": result}


def _error(id: Any, code: int, message: str) -> Dict[str, Any]:
    return {"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}


def _within(cwd: str, roots: List[str]) -> bool:
    """Whether `cwd` is one of `roots` or under one; any is, with none given."""
    if not roots:
        return True
    for root in roots:
        root = root.rstrip("/\\")
        if cwd == root or (cwd.startswith(root) and cwd[len(root)] in "/\\"):
            return True
    return False


class _Agent:
    """The ACP side: what each message from the client is answered with."""

    def __init__(self, roots: List[str]) -> None:
        self.roots = roots
        self.sessions = 0

    def answer(self, data: bytes) -> Tuple[List[Dict[str, Any]], bool]:
        """The messages to send back, and whether the connection ends after them."""
        try:
            message = json.loads(data)
        except ValueError:
            return [_error(None, -32700, "not a JSON-RPC message")], False
        if not isinstance(message, dict) or "method" not in message or "id" not in message:
            # A notification (`session/cancel` among them: every turn ends at once) or an answer.
            return [], False
        id = message["id"]
        params = message.get("params") or {}
        method = message["method"]
        if method == "initialize":
            return [_result(id, {
                "protocolVersion": 1,
                "agentCapabilities": {
                    "loadSession": False,
                    "promptCapabilities": {},
                    "mcpCapabilities": {"http": False, "sse": False},
                },
                "authMethods": [],
                "agentInfo": {"name": "acp_echo_harness", "title": "Echo, in Python", "version": __version__},
            })], False
        if method == "session/new":
            if not _within(str(params.get("cwd", "")), self.roots):
                return [_error(id, -32602, "the cwd is outside the working roots")], False
            self.sessions += 1
            return [_result(id, {"sessionId": f"python-echo-{self.sessions}"})], False
        if method == "session/prompt":
            text = "".join(
                block.get("text", "") for block in params.get("prompt", []) if isinstance(block, dict)
            )
            update = {
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": params.get("sessionId"),
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type": "text", "text": f"echo: {text}"},
                    },
                },
            }
            return [update, _result(id, {"stopReason": "end_turn"})], text == "close"
        return [_error(id, -32601, "method not found")], False


def _deliver(sink: ChannelSink, q: _Queue, roots: List[str]) -> None:
    """The connection's thread, and the only caller of the sink."""
    agent = _Agent(roots)
    status = 0
    try:
        while True:
            with q.cond:
                while not q.items and not q.closed:
                    q.cond.wait()
                if q.items:
                    message: Optional[bytes] = q.items.popleft()
                    q.cond.notify_all()
                else:
                    message = None
                    status = q.host_status if q.host_status is not None else 0
            if message is None:
                q.end()
                break
            answers, then_close = agent.answer(message)
            refused = None
            for answer in answers:
                try:
                    sink.receive(json.dumps(answer).encode("utf-8"))
                except Exception as e:  # any refusal ends the connection, once
                    refused = int(getattr(e, "status", INTERNAL))
                    break
            if refused is not None:
                status = refused
                q.end()
                break
            if then_close:
                q.end()
                break
        sink.closed(status)
    finally:
        sink.destroy()


class EchoHarness(provide.Provider, AcpHarness, display_name="Echo harness, in Python"):
    """Starts an echoing ACP connection per call."""

    def describe(self) -> Dict[str, Any]:
        return {"title": "Echo, in Python", "versions": [1], "takes": {}}

    def start(self, request: Ref, sink: ChannelSink) -> Channel:
        asked = request.to_python()
        if any(r["name"] not in HOLDS for r in asked.get("requirements", [])):
            # A refused start still lets go of the sink it was handed.
            sink.destroy()
            raise Unsupported("the echo cannot hold every requirement asked")
        q = _Queue()
        threading.Thread(
            target=_deliver,
            args=(sink, q, list(asked.get("working_roots", []))),
            name="acp_echo_harness",
            daemon=True,
        ).start()
        return Connection(q)

    def refusal(self, requirement: Ref, environment: Ref) -> Optional[str]:
        if requirement.to_python()["name"] in HOLDS:
            return None
        return "the echo does not know this requirement"

    def models(self, environment: Ref) -> Dict[str, Any]:
        return {"known": True, "models": [{"id": "echo"}]}
