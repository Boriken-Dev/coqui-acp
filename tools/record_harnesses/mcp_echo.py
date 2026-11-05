"""A stdio MCP server with one tool, `echo`, for recordings: newline-delimited JSON-RPC."""

import json
import sys

TOOL = {"name": "echo", "description": "Says the text back.",
        "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}}

for line in sys.stdin:
    try:
        message = json.loads(line)
    except ValueError:
        continue
    if "id" not in message:
        continue
    params = message.get("params", {})
    result = {
        "initialize": {"protocolVersion": params.get("protocolVersion", "2025-06-18"), "capabilities": {"tools": {}},
                       "serverInfo": {"name": "echo", "version": "0"}},
        "tools/list": {"tools": [TOOL]},
        "tools/call": {"content": [{"type": "text", "text": "echo: " + params.get("arguments", {}).get("text", "")}]},
    }.get(message.get("method"), {})
    print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result}), flush=True)
