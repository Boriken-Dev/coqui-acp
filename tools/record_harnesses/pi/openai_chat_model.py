#!/usr/bin/env python3
"""Loopback stand-in for the OpenAI chat-completions API that pi calls.

Usage: openai_chat_model.py PORT LOGFILE

Pi is pointed at it through models.json (provider "stub", api "openai-completions",
baseUrl http://127.0.0.1:PORT/v1, apiKey "sk-test"). The reply depends on the last
user message or on the tool result that ends the request; markers are written in
the prompt as [[name]] or [[name:argument]]:

  (none)            streamed text "Hello from the stand-in."
  [[think]]         reasoning_content deltas, then text
  [[read:PATH]]     one tool call: read {path}
  [[write:PATH]]    one tool call: write {path, content}
  [[edit:PATH]]     one tool call: edit {path, edits:[{oldText,newText}]}
  [[bash:CMD]]      one tool call: bash {command}
  [[two:PATH]]      two tool calls in one assistant message: read PATH, ls
  [[slow]]          text deltas 0.25 s apart for 30 s (to abort mid-turn)
  [[429]] [[500]]   HTTP error with a JSON body
  [[429once]]       429 the first time this marker is seen, then text
  [[drop]]          closes the connection after the headers (cut stream)
  [[huge]]          reports 60000 prompt tokens in usage (to trigger compaction)

A request whose last message has role "tool" is answered with text that echoes
the start of the tool result. A request whose system prompt asks for a
summary (compaction) is answered with a short summary text.

Every request body is appended to LOGFILE as one JSON line (headers' Authorization
value replaced by "<redacted>"), so a translator author can see what pi sends,
including how an image and a thinking level reach the provider.
"""
import json
import re
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1])
LOG = sys.argv[2]
LOCK = threading.Lock()
SEEN_ONCE = set()
MARK = re.compile(r"\[\[(\w+)(?::([^\]]*))?\]\]")


def log(entry):
    with LOCK:
        with open(LOG, "a", encoding="utf-8", newline="") as f:
            f.write(json.dumps(entry, ensure_ascii=False) + "\n")


def text_of(content):
    if isinstance(content, str):
        return content
    out = []
    for part in content or []:
        if isinstance(part, dict) and part.get("type") == "text":
            out.append(part.get("text", ""))
    return "\n".join(out)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def send_json(self, status, obj):
        body = json.dumps(obj).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        log({"method": "GET", "path": self.path})
        if self.path.rstrip("/").endswith("/models"):
            self.send_json(200, {"object": "list", "data": [{"id": "stub-fast", "object": "model"}]})
        else:
            self.send_json(404, {"error": {"message": "not found"}})

    def sse(self, obj):
        self.wfile.write(b"data: " + json.dumps(obj).encode() + b"\n\n")
        self.wfile.flush()

    def do_POST(self):
        n = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(n)
        try:
            req = json.loads(raw)
        except Exception:
            req = {"_unparsed": raw.decode("utf-8", "replace")}
        headers = {k: ("<redacted>" if k.lower() == "authorization" else v) for k, v in self.headers.items()}
        log({"method": "POST", "path": self.path, "headers": headers, "body": req})
        msgs = req.get("messages", [])
        last = msgs[-1] if msgs else {}
        system = text_of(msgs[0].get("content")) if msgs and msgs[0].get("role") in ("system", "developer") else ""
        marker, arg = None, ""
        if last.get("role") == "user":
            m = MARK.search(text_of(last.get("content")))
            if m:
                marker, arg = m.group(1), m.group(2) or ""
        model = req.get("model", "stub-fast")
        base = {"id": "chatcmpl-stub", "object": "chat.completion.chunk", "created": 1, "model": model}

        if marker in ("429", "500"):
            self.send_json(int(marker), {"error": {"message": "stand-in forced %s" % marker, "type": "stub_error", "code": marker}})
            return
        if marker == "429once":
            with LOCK:
                first = "429once" not in SEEN_ONCE
                SEEN_ONCE.add("429once")
            if first:
                self.send_response(429)
                self.send_header("Retry-After", "1")
                body = json.dumps({"error": {"message": "stand-in rate limit", "type": "rate_limit"}}).encode()
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return
            marker = None
        if marker == "drop":
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            self.wfile.flush()
            self.close_connection = True
            return

        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.end_headers()
        self.close_connection = True

        def chunk(delta, finish=None):
            c = dict(base)
            c["choices"] = [{"index": 0, "delta": delta, "finish_reason": finish}]
            self.sse(c)

        def usage(prompt, completion):
            c = dict(base)
            c["choices"] = []
            c["usage"] = {"prompt_tokens": prompt, "completion_tokens": completion, "total_tokens": prompt + completion}
            self.sse(c)

        prompt_tokens = 60000 if marker == "huge" else 25
        try:
            if "summar" in system.lower() and last.get("role") == "user" and "summar" in text_of(last.get("content")).lower() and marker is None:
                chunk({"role": "assistant", "content": ""})
                chunk({"content": "## Goal\nStand-in summary of the conversation.\n"})
                chunk({}, "stop")
                usage(300, 12)
            elif last.get("role") == "tool":
                chunk({"role": "assistant", "content": ""})
                echo = text_of(last.get("content")).replace("\n", " ")[:80]
                for piece in ["Tool finished. ", "Result begins: ", echo]:
                    chunk({"content": piece})
                chunk({}, "stop")
                usage(prompt_tokens + 20, 9)
            elif marker in ("read", "write", "edit", "bash", "two"):
                chunk({"role": "assistant", "content": None})
                calls = []
                if marker == "read":
                    calls = [("call_1", "read", {"path": arg})]
                elif marker == "write":
                    calls = [("call_1", "write", {"path": arg, "content": "written by stand-in\nline two\n"})]
                elif marker == "edit":
                    calls = [("call_1", "edit", {"path": arg, "edits": [{"oldText": "alpha", "newText": "ALPHA"}]})]
                elif marker == "bash":
                    calls = [("call_1", "bash", {"command": arg})]
                elif marker == "two":
                    calls = [("call_1", "read", {"path": arg}), ("call_2", "ls", {"path": "."})]
                for i, (cid, name, args) in enumerate(calls):
                    s = json.dumps(args)
                    chunk({"tool_calls": [{"index": i, "id": cid, "type": "function", "function": {"name": name, "arguments": ""}}]})
                    half = len(s) // 2
                    for part in (s[:half], s[half:]):
                        chunk({"tool_calls": [{"index": i, "function": {"arguments": part}}]})
                chunk({}, "tool_calls")
                usage(prompt_tokens, 30)
            elif marker == "slow":
                chunk({"role": "assistant", "content": ""})
                for i in range(120):
                    chunk({"content": "tick%d " % i})
                    time.sleep(0.25)
                chunk({}, "stop")
                usage(prompt_tokens, 120)
            elif marker == "think":
                chunk({"role": "assistant", "content": ""})
                for piece in ["Let me ", "think about it. "]:
                    chunk({"reasoning_content": piece})
                for piece in ["The answer ", "is 42."]:
                    chunk({"content": piece})
                chunk({}, "stop")
                usage(prompt_tokens, 15)
            else:
                chunk({"role": "assistant", "content": ""})
                for piece in ["Hello ", "from the ", "stand-in."]:
                    chunk({"content": piece})
                chunk({}, "stop")
                usage(prompt_tokens, 6)
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError, OSError):
            log({"note": "client went away mid-stream", "marker": marker})


if __name__ == "__main__":
    srv = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    srv.daemon_threads = True
    print("stand-in listening on 127.0.0.1:%d" % PORT, flush=True)
    srv.serve_forever()
