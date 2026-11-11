#!/usr/bin/env python3
"""Access requests for a private iroh relay.

The relay calls POST /authorize for every client; this service answers 200
"true" only for approved endpoint ids. Users POST /access/request with their
endpoint id through Traefik. Only the operator can approve, by running this
script inside the container over SSH.

State is two JSON arrays under /state (or STATE_DIR).
"""

import json
import os
import re
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

STATE_DIR = os.environ.get("STATE_DIR", "/state")
APPROVED = os.path.join(STATE_DIR, "approved.json")
PENDING = os.path.join(STATE_DIR, "pending.json")
ID_RE = re.compile(r"^[0-9a-f]{64}$")
MAX_PENDING = 512
MAX_BODY = 4096
LOCK = threading.Lock()
PAIR_TTL = 600
MAX_PAIRS = 64
PAIR_LOCK = threading.Lock()
PAIRS = {}


def load(path):
    try:
        with open(path) as handle:
            data = json.load(handle)
        return data if isinstance(data, list) else []
    except (FileNotFoundError, ValueError, OSError):
        return []


def save(path, data):
    tmp = path + ".tmp"
    with open(tmp, "w") as handle:
        json.dump(data, handle)
    os.replace(tmp, path)


def approved():
    return load(APPROVED)


def pending():
    return load(PENDING)


def valid(endpoint_id):
    return bool(endpoint_id and ID_RE.match(endpoint_id))


def purge_pairs():
    now = time.monotonic()
    for key in [key for key, (expires, _) in PAIRS.items() if expires <= now]:
        PAIRS.pop(key, None)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        sys.stderr.write("access: " + fmt % args + "\n")

    def reply(self, code, body, content_type="application/json"):
        raw = body.encode()
        self.send_response(code)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(raw)))
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        self.send_header("Access-Control-Allow-Headers", "Content-Type")
        self.end_headers()
        self.wfile.write(raw)

    def do_OPTIONS(self):
        self.reply(204, "")

    def do_GET(self):
        parsed = urlparse(self.path)
        if parsed.path == "/health":
            self.reply(200, "ok", "text/plain")
            return
        if parsed.path == "/access/status":
            endpoint_id = parse_qs(parsed.query).get("id", [""])[0]
            if not valid(endpoint_id):
                self.reply(400, json.dumps({"error": "invalid endpoint id"}))
                return
            with LOCK:
                if endpoint_id in approved():
                    status = "approved"
                elif endpoint_id in pending():
                    status = "pending"
                else:
                    status = "unknown"
            self.reply(200, json.dumps({"status": status}))
            return
        self.reply(404, json.dumps({"error": "not found"}))

    def do_POST(self):
        parsed = urlparse(self.path)
        if parsed.path == "/authorize":
            endpoint_id = self.headers.get("X-Iroh-Endpoint-Id", "")
            if not valid(endpoint_id):
                endpoint_id = self.headers.get("X-Iroh-NodeId", "")
            with LOCK:
                allowed = valid(endpoint_id) and endpoint_id in approved()
            if allowed:
                self.reply(200, "true", "text/plain")
            else:
                self.reply(403, "false", "text/plain")
            return
        if parsed.path == "/access/request":
            try:
                length = int(self.headers.get("Content-Length", "0"))
            except ValueError:
                length = 0
            if length <= 0 or length > MAX_BODY:
                self.reply(400, json.dumps({"error": "invalid body"}))
                return
            try:
                body = json.loads(self.rfile.read(length))
                endpoint_id = body.get("endpointId", "")
            except (ValueError, AttributeError):
                self.reply(400, json.dumps({"error": "invalid json"}))
                return
            if not valid(endpoint_id):
                self.reply(400, json.dumps({"error": "invalid endpoint id"}))
                return
            with LOCK:
                if endpoint_id in approved():
                    self.reply(200, json.dumps({"status": "approved"}))
                    return
                waiting = pending()
                if endpoint_id in waiting:
                    self.reply(200, json.dumps({"status": "pending"}))
                    return
                if len(waiting) >= MAX_PENDING:
                    self.reply(429, json.dumps({"error": "request queue full"}))
                    return
                waiting.append(endpoint_id)
                save(PENDING, waiting)
            self.reply(202, json.dumps({"status": "pending"}))
            return
        if parsed.path in {"/pair", "/pair/take"}:
            try:
                length = int(self.headers.get("Content-Length", "0"))
            except ValueError:
                length = 0
            if length <= 0 or length > 32768:
                self.reply(400, json.dumps({"error": "invalid body"}))
                return
            try:
                body = json.loads(self.rfile.read(length))
                lookup = body.get("lookup", "")
            except (ValueError, AttributeError):
                self.reply(400, json.dumps({"error": "invalid json"}))
                return
            if not valid(lookup):
                self.reply(400, json.dumps({"error": "invalid lookup"}))
                return
            if parsed.path == "/pair":
                blob = body.get("blob")
                if not isinstance(blob, str) or not blob or len(blob) > 16384:
                    self.reply(400, json.dumps({"error": "invalid blob"}))
                    return
                with PAIR_LOCK:
                    purge_pairs()
                    if lookup not in PAIRS and len(PAIRS) >= MAX_PAIRS:
                        self.reply(429, json.dumps({"error": "pairing queue full"}))
                        return
                    PAIRS[lookup] = (time.monotonic() + PAIR_TTL, blob)
                self.reply(201, json.dumps({"ok": True}))
                return
            with PAIR_LOCK:
                purge_pairs()
                entry = PAIRS.pop(lookup, None)
            if entry is None:
                self.reply(404, json.dumps({"error": "not found or expired"}))
                return
            self.reply(200, json.dumps({"blob": entry[1]}))
            return
        self.reply(404, json.dumps({"error": "not found"}))


def cli(args):
    command = args[0] if args else ""
    if command == "pending":
        for endpoint_id in pending():
            print(endpoint_id)
        return 0
    if command == "list":
        for endpoint_id in approved():
            print(endpoint_id)
        return 0
    if command in {"approve", "deny", "revoke"} and len(args) == 2:
        endpoint_id = args[1].strip()
        if not valid(endpoint_id):
            print("invalid endpoint id", file=sys.stderr)
            return 2
        with LOCK:
            waiting = pending()
            allowed = approved()
            if command == "approve":
                if endpoint_id in waiting:
                    waiting.remove(endpoint_id)
                if endpoint_id not in allowed:
                    allowed.append(endpoint_id)
                save(PENDING, waiting)
                save(APPROVED, allowed)
                print("approved " + endpoint_id)
            elif command == "deny":
                if endpoint_id in waiting:
                    waiting.remove(endpoint_id)
                    save(PENDING, waiting)
                    print("denied " + endpoint_id)
                else:
                    print("not pending", file=sys.stderr)
                    return 1
            else:
                if endpoint_id in allowed:
                    allowed.remove(endpoint_id)
                    save(APPROVED, allowed)
                    print("revoked " + endpoint_id)
                else:
                    print("not approved", file=sys.stderr)
                    return 1
        return 0
    print(
        "usage: access.py [pending | list | approve <id> | deny <id> | revoke <id>]",
        file=sys.stderr,
    )
    return 2


def main():
    os.makedirs(STATE_DIR, exist_ok=True)
    if len(sys.argv) > 1:
        raise SystemExit(cli(sys.argv[1:]))
    server = ThreadingHTTPServer(("0.0.0.0", 8090), Handler)
    server.serve_forever()


if __name__ == "__main__":
    main()
