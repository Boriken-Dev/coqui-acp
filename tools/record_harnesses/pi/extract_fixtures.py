"""Turns the transcripts record_pi.py writes into the crate's fixtures: the lines Pi wrote, one JSON object per line.

Dropped: Pi's own system message (events and entries of get_messages / agent_end.messages), agent_end's message list.
permission_gate is rewritten to the dialog titles of the gate this crate ships.
"""
import json
import re
import sys
from pathlib import Path

SRC = Path(sys.argv[1])  # the recorder's output directory
DST = Path(sys.argv[2])
SCENARIOS = [
    "startup_state", "models_thinking", "prompt_text", "prompt_thinking", "prompt_image", "tool_read",
    "tool_read_image", "tool_write", "tool_edit", "tool_bash", "tool_two", "abort_turn", "abort_tool", "errors",
    "retry_abort", "permission_gate", "compaction", "session", "rpc_bash", "ui_methods", "no_credentials", "queue",
]
GATE = re.compile(r'^Allow (\w+) \{')


def keep(message):
    return not (isinstance(message, dict) and message.get("role") == "system")


def clean(value):
    kind = value.get("type")
    if kind in ("message_start", "message_end") and not keep(value.get("message")):
        return None
    if kind == "agent_end":
        value["messages"] = []
    if kind == "response":
        data = value.get("data")
        if isinstance(data, dict) and isinstance(data.get("messages"), list):
            data["messages"] = [m for m in data["messages"] if keep(m)]
    if kind == "extension_ui_request" and value.get("method") == "select":
        title = value.get("title", "")
        found = GATE.match(title)
        if found and value.get("options") == ["Allow once", "Reject"]:
            tool = found.group(1)
            value["title"] = "coqui-acp-pi-gate " + json.dumps({"id": "call_1", "tool": tool}, separators=(",", ":"))
            value["options"] = ["allow", "deny", "plan"]
    return value


def main():
    DST.mkdir(parents=True, exist_ok=True)
    total = 0
    for name in SCENARIOS:
        lines = []
        for raw in (SRC / f"{name}.txt").read_text(encoding="utf-8").splitlines():
            if not raw.startswith("< "):
                continue
            try:
                value = json.loads(raw[2:])
            except ValueError:
                continue
            value = clean(value)
            if value is not None:
                lines.append(json.dumps(value, separators=(",", ":"), ensure_ascii=False))
        text = "\n".join(lines) + "\n"
        (DST / f"{name}.stdout").write_bytes(text.encode("utf-8"))
        total += len(text)
        print(name, len(lines), len(text))
    print("total", total)


main()
