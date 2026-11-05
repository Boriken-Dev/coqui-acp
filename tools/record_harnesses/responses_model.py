"""A stand-in for the OpenAI Responses API and one MCP tool on 127.0.0.1: no model is reached.

    python3 -I responses_model.py PORT_FILE LOG_FILE SCRIPT_JSON

Each POST to /v1/responses takes the next step of the script; once it is used up the answer is "done". A step has any of:

  "reasoning": text   a reasoning item with this summary, first
  "text": text        an assistant message, streamed in words
  "exec": javascript  a code-mode `exec` custom tool call
  "call": item        any output item, as given
  "hold": seconds     wait this long first
  "status": code      answer with this HTTP status instead; "kind" and "message" say the error

The MCP endpoint is /mcp, with one tool, `echo`.
"""

import json
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT_FILE, LOG_FILE, SCRIPT_FILE = sys.argv[1:4]
with open(SCRIPT_FILE, encoding="utf-8") as script_file:
    SCRIPT = json.load(script_file)
LOCK = threading.Lock()
STATE = {"step": 0, "seq": 0}
ECHO = {"name": "echo", "description": "Says the text back.",
        "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}}


def log(record):
    with LOCK, open(LOG_FILE, "a", encoding="utf-8", newline="") as handle:
        handle.write(json.dumps(record) + "\n")


def body_of(raw, encoding):
    """The request's JSON: Codex compresses what it sends with zstd, which Python has from 3.14."""
    if encoding == "zstd":
        from compression import zstd
        raw = zstd.decompress(raw)
    elif encoding == "gzip":
        import gzip
        raw = gzip.decompress(raw)
    try:
        return json.loads(raw)
    except ValueError:
        return {}


def events_of(seq, step):
    response = f"resp_{seq}"
    events = [{"type": "response.created", "response": {"id": response}}]
    index = 0
    if "reasoning" in step:
        item = f"rs_{seq}"
        events += [
            {"type": "response.output_item.added", "output_index": index, "item": {"type": "reasoning", "id": item, "summary": []}},
            {"type": "response.reasoning_summary_part.added", "item_id": item, "output_index": index, "summary_index": 0,
             "part": {"type": "summary_text", "text": ""}},
            {"type": "response.reasoning_summary_text.delta", "item_id": item, "output_index": index, "summary_index": 0,
             "delta": step["reasoning"]},
            {"type": "response.output_item.done", "output_index": index,
             "item": {"type": "reasoning", "id": item, "summary": [{"type": "summary_text", "text": step["reasoning"]}]}},
        ]
        index += 1
    if "text" in step:
        item = f"msg_{seq}"
        events += [
            {"type": "response.output_item.added", "output_index": index,
             "item": {"type": "message", "role": "assistant", "id": item, "content": []}},
            {"type": "response.content_part.added", "item_id": item, "output_index": index, "content_index": 0,
             "part": {"type": "output_text", "text": ""}},
        ]
        for at, word in enumerate(step["text"].split(" ")):
            events.append({"type": "response.output_text.delta", "item_id": item, "output_index": index, "content_index": 0,
                           "delta": word if at == 0 else " " + word})
        events.append({"type": "response.output_item.done", "output_index": index,
                       "item": {"type": "message", "role": "assistant", "id": item,
                                "content": [{"type": "output_text", "text": step["text"]}]}})
        index += 1
    if "exec" in step:
        events.append({"type": "response.output_item.done", "output_index": index,
                       "item": {"type": "custom_tool_call", "call_id": f"call_{seq}", "name": "exec", "input": step["exec"],
                                "status": "completed", "id": f"ctc_{seq}"}})
        index += 1
    if "call" in step:
        events.append({"type": "response.output_item.done", "output_index": index,
                       "item": {"call_id": f"call_{seq}", **step["call"]}})
    events.append({"type": "response.completed", "response": {"id": response, "usage": {
        "input_tokens": 21, "input_tokens_details": {"cached_tokens": 5}, "output_tokens": 9,
        "output_tokens_details": {"reasoning_tokens": 2}, "total_tokens": 30}}})
    return "".join(f"event: {event['type']}\ndata: {json.dumps(event)}\n\n" for event in events).encode("utf-8")


def mcp_result(method, params):
    if method == "initialize":
        return {"protocolVersion": params.get("protocolVersion", "2025-06-18"), "capabilities": {"tools": {}},
                "serverInfo": {"name": "echo", "version": "0"}}
    if method == "tools/list":
        return {"tools": [ECHO]}
    if method == "tools/call":
        return {"content": [{"type": "text", "text": "echo: " + params.get("arguments", {}).get("text", "")}]}
    return {}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def reply(self, status, data, content_type="application/json"):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        log({"method": "GET", "path": self.path})
        if self.path.endswith("/models"):
            return self.reply(200, b'{"models": [], "data": []}')
        self.reply(404, b"{}")

    def do_DELETE(self):
        self.reply(200, b"{}")

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        body = body_of(self.rfile.read(length), self.headers.get("Content-Encoding"))
        if "/mcp" in self.path:
            log({"method": "MCP", "rpc": body.get("method")})
            if "id" not in body:
                return self.reply(202, b"")
            result = mcp_result(body.get("method"), body.get("params", {}))
            return self.reply(200, json.dumps({"jsonrpc": "2.0", "id": body["id"], "result": result}).encode("utf-8"))
        with LOCK:
            STATE["seq"] += 1
            seq = STATE["seq"]
            at = STATE["step"]
            STATE["step"] = at + 1
        step = SCRIPT[at] if at < len(SCRIPT) else {"text": "done"}
        log({"method": "POST", "path": self.path, "seq": seq, "model": body.get("model"),
             "instructions": (body.get("instructions") or "")[:200],
             "tools": [tool.get("name") for tool in body.get("tools", []) if isinstance(tool, dict)]})
        time.sleep(step.get("hold", 0))
        if "status" in step:
            refusal = {"error": {"message": step.get("message", "stand-in refusal"), "type": step.get("kind", "server_error")}}
            return self.reply(step["status"], json.dumps(refusal).encode("utf-8"))
        self.reply(200, events_of(seq, step), "text/event-stream")


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
with open(PORT_FILE, "w", encoding="utf-8") as port_file:
    port_file.write(str(server.server_address[1]))
server.serve_forever()
