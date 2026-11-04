"""An agent that answers each message with its text, as a Guatiao library written in Python.

The module is the library: its id is its name, its version `__version__`, its providers the classes in
`__all__`. A prompt beginning with a word below shows what a provider may do.
"""

from __future__ import annotations

import json
import queue
from typing import Any, Dict, Optional

from coqui_acp_provider_kinds import AgentProvider, AgentSession

from guatiao import BadValue, Ref, provide

__version__ = "1.0.0"
__all__ = ["Echo"]


class Conversation(AgentSession):
    """One conversation. Python runs one call at a time, and `queue` lets go of it while it waits."""

    def __init__(self, identity: str, request: Dict[str, Any]) -> None:
        self.identity = identity
        self.request = request
        self.events: "queue.Queue[Dict[str, Any]]" = queue.Queue()
        self.closed = False

    def answer(self, text: str) -> None:
        """What a message makes the agent report."""
        if text == "show":
            self.events.put({"kind": "text", "text": json.dumps(self.request, sort_keys=True)})
            self.events.put({"kind": "completed"})
        elif text == "tool":
            self.events.put({"kind": "tool", "text": "Read a file", "tools": ["Read"]})
            self.events.put({"kind": "usage", "usage": {
                "input": 1, "output": 2, "cache_read": 0, "cache_write": 0,
                "cost_micro_usd": 1500000, "cumulative": True,
            }})
            self.events.put({"kind": "completed"})
        elif text == "wait":
            self.events.put({"kind": "text", "text": "waiting"})
        elif text == "fail":
            self.events.put({"kind": "failed", "text": "the harness exited with 3"})
        elif text == "raise":
            raise BadValue("the message was refused")
        else:
            self.events.put({"kind": "text", "text": f"echo: {text}"})
            self.events.put({"kind": "completed"})

    def session_id(self) -> Optional[str]:
        return self.identity

    def send_message(self, message: Ref) -> None:
        self.answer(message.to_python()["text"])

    def next_event(self, timeout_ms: int) -> Optional[Dict[str, Any]]:
        try:
            return self.events.get(timeout=timeout_ms / 1000)
        except queue.Empty:
            return None

    def interrupt(self) -> None:
        self.events.put({"kind": "cancelled"})

    def close(self) -> None:
        self.closed = True


class Echo(provide.Provider, AgentProvider, display_name="Echo, in Python"):
    """Starts conversations, and resumes any whose identity it issued."""

    def start(self, request: Ref) -> Conversation:
        asked = request.to_python()
        if asked["prompt"] == "refuse":
            raise BadValue("this agent starts nothing for that")
        conversation = Conversation("python-echo-1", asked)
        conversation.answer(asked["prompt"])
        return conversation

    def resume(self, request: Ref) -> Conversation:
        asked = request.to_python()
        if not asked["session_id"].startswith("python-echo-"):
            raise BadValue("this provider issued no such session")
        return Conversation(asked["session_id"], asked)
