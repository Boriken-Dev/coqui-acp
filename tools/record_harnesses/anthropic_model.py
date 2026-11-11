"""A stand-in for the Anthropic Messages API on 127.0.0.1: no model is reached.

    python3 -I anthropic_model.py PORT_FILE LOG_FILE SCRIPT_JSON

Each POST to /v1/messages that offers tools, the main loop's, takes the next step of the script; any other (titles,
summaries) is answered "ok", and once the script is used up the answer is "done". A step has any of:

  "thinking": text              a thinking block first
  "text": text                  a text block, streamed in words
  "tools": [[name, input], ...] tool_use blocks, ending with stop_reason tool_use
  "hold": seconds               wait this long first
  "status": code                answer with this HTTP status instead; "kind" and "message" say the error
  "headers": {name: value}      more response headers, such as the unified rate limit ones
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


def log(record):
    with LOCK, open(LOG_FILE, "a", encoding="utf-8", newline="") as handle:
        handle.write(json.dumps(record) + "\n")


def sse(data):
    return f"event: {data['type']}\ndata: {json.dumps(data)}\n\n"


def delta(index, body):
    return sse({"type": "content_block_delta", "index": index, "delta": body})


def block(index, content_block, deltas):
    start = sse({"type": "content_block_start", "index": index, "content_block": content_block})
    return start + "".join(delta(index, body) for body in deltas) + sse({"type": "content_block_stop", "index": index})


def stream(message_id, step):
    parts = [sse({"type": "message_start", "message": {
        "id": message_id, "type": "message", "role": "assistant", "model": "claude-standin", "content": [],
        "stop_reason": None, "stop_sequence": None,
        "usage": {"input_tokens": 11, "output_tokens": 1, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}}})]
    index = 0
    if "thinking" in step:
        parts.append(block(index, {"type": "thinking", "thinking": "", "signature": ""}, [
            {"type": "thinking_delta", "thinking": step["thinking"]},
            {"type": "signature_delta", "signature": "c3RhbmQtaW4="}]))
        index += 1
    if "text" in step:
        words = step["text"].split(" ")
        pieces = [word if at == 0 else " " + word for at, word in enumerate(words)]
        parts.append(block(index, {"type": "text", "text": ""}, [{"type": "text_delta", "text": piece} for piece in pieces]))
        index += 1
    for name, tool_input in step.get("tools", []):
        tool_id = f"toolu_standin_{STATE['seq']:03d}_{index}"
        parts.append(block(index, {"type": "tool_use", "id": tool_id, "name": name, "input": {}},
                           [{"type": "input_json_delta", "partial_json": json.dumps(tool_input)}]))
        index += 1
    reason = "tool_use" if step.get("tools") else "end_turn"
    parts.append(sse({"type": "message_delta", "delta": {"stop_reason": reason, "stop_sequence": None}, "usage": {"output_tokens": 7}}))
    parts.append(sse({"type": "message_stop"}))
    return "".join(parts)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def reply(self, status, body, content_type="application/json", headers=None):
        data = body.encode("utf-8")
        self.send_response(status)
        for name, value in (headers or {}).items():
            self.send_header(name, value)
        self.send_header("content-type", content_type)
        self.send_header("content-length", str(len(data)))
        self.send_header("connection", "close")
        self.end_headers()
        self.wfile.write(data)

    def refuse(self, status, kind, message, headers=None):
        self.reply(status, json.dumps({"type": "error", "error": {"type": kind, "message": message}}), headers=headers)

    def do_HEAD(self):
        self.send_response(200)
        self.send_header("content-length", "0")
        self.end_headers()

    def do_GET(self):
        log({"method": "GET", "path": self.path})
        self.refuse(404, "not_found_error", "stand-in")

    def do_POST(self):
        raw = self.rfile.read(int(self.headers.get("content-length") or 0))
        try:
            body = json.loads(raw)
        except ValueError:
            body = {}
        with LOCK:
            STATE["seq"] += 1
            seq = STATE["seq"]
        main = bool(body.get("tools")) and "count_tokens" not in self.path
        log({"method": "POST", "path": self.path, "seq": seq, "main": main, "system": body.get("system"),
             "tools": [tool.get("name") for tool in body.get("tools", [])], "model": body.get("model"),
             "messages": len(body.get("messages", [])), "input": body.get("messages")})
        if "count_tokens" in self.path:
            return self.reply(200, json.dumps({"input_tokens": 1}))
        if not self.path.startswith("/v1/messages"):
            return self.refuse(404, "not_found_error", "stand-in")
        step = {"text": "ok"}
        if main:
            with LOCK:
                step = SCRIPT[STATE["step"]] if STATE["step"] < len(SCRIPT) else {"text": "done"}
                STATE["step"] += 1
        time.sleep(step.get("hold", 0))
        if "status" in step:
            kind = step.get("kind") or {429: "rate_limit_error", 529: "overloaded_error"}.get(step["status"], "api_error")
            return self.refuse(step["status"], kind, step.get("message", "stand-in refusal"), step.get("headers"))
        message_id = f"msg_standin_{seq:03d}"
        if body.get("stream"):
            return self.reply(200, stream(message_id, step), "text/event-stream", step.get("headers"))
        content = [{"type": "text", "text": step["text"]}] if "text" in step else [{"type": "text", "text": "ok"}]
        self.reply(200, json.dumps({"id": message_id, "type": "message", "role": "assistant", "model": "claude-standin",
                                    "content": content, "stop_reason": "end_turn", "stop_sequence": None,
                                    "usage": {"input_tokens": 1, "output_tokens": 1}}), headers=step.get("headers"))


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
with open(PORT_FILE, "w", encoding="utf-8") as port_file:
    port_file.write(str(server.server_address[1]))
server.serve_forever()
