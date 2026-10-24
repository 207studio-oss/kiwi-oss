#!/usr/bin/env python3
"""kiwi-live: Streamable HTTP transport for kiwi-sprouts with bearer-token auth.

Kept out of kiwi_sprouts.py so the stdio server stays listener-free.
Usage: python3 kiwi_live.py [--root R] [--host 127.0.0.1] [--port 8765]
Token: KIWI_LIVE_TOKEN_FILE or ~/.kiwi/secrets/kiwi-live-token (chmod 600, >=32 chars).
See decisions/2026-10-24-kiwi-live-access.md.
"""
import json, os, sys, threading
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from kiwi_sprouts import Sprouts, handle, KIWI
from kiwi_brief import GUARDED_TOOLS
from kiwi_events import EventBus, SECRET_LINE
import kiwi_dsh_host
import re, subprocess, time, datetime

HTTP_TOOLS = ("ask", "ask_resolve", "ask_stats", "context_for", "kiwi_rule", "kiwi_status", "task_guard", "docs_search", "decide",
              "record_lesson", "record_decision",
              "rule_proposals", "brief_feedback", "brief_quality", "decide_calibration",
              "claim_register", "claim_renew", "claim_release", "claim_list",
              "subscribe", "unsubscribe", "subscriptions", "tokens_get", "token_set", "dsh_host")

# Remote callers are outside LLMs: secrets and student data never leave the box (rules/bot-identity: no key originals to external LLMs).
REMOTE_DENY_DIRS = {"accounts", "secrets", "evidence", "inbox", "uncollected", "life", ".git", "assets/account", "sales/accounts"}
STUDENT = re.compile(r"student|학생|hwp|sample-app|chk|teachers|teacher", re.I)
RULE_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._/-]{0,200}$")
COSTLY = {"ask", "decide", "context_for"}


def remote_path_denied(rel):
    rel = str(rel).replace("\\", "/").removeprefix("./")
    if ".." in rel.split("/") or rel.startswith("/"): return True
    if any(rel == d or rel.startswith(d + "/") for d in REMOTE_DENY_DIRS): return True
    if any(p in {".git", "secrets"} or p.startswith(".env") or p.endswith((".key", ".pem")) for p in rel.split("/")): return True
    return bool(STUDENT.search(rel))


def scrub(v):
    """Recursively drop secret-looking lines from any string in a result."""
    if isinstance(v, str):
        return "\n".join("[redacted]" if SECRET_LINE.search(l) else l for l in v.split("\n"))
    if isinstance(v, list): return [scrub(x) for x in v]
    if isinstance(v, dict): return {k: scrub(x) for k, x in v.items()}
    return v


class Budget:
    """Per-minute request limit plus a daily cap on paid (Decisions/OpenAI) calls; persisted across restarts."""
    def __init__(self, path=None, per_min=None, daily=None):
        self.path = Path(os.path.expanduser(path or os.environ.get("KIWI_LIVE_USAGE") or "~/.kiwi/state/kiwi-live-usage.json"))
        self.per_min = int(per_min or os.environ.get("KIWI_LIVE_PER_MIN", 60))
        self.daily = int(daily if daily is not None else os.environ.get("KIWI_LIVE_DAILY_PAID", 300))
        self.window = []; self.lock = threading.Lock()

    def _load(self):
        try: d = json.loads(self.path.read_text())
        except Exception: d = {}
        today = datetime.date.today().isoformat()
        return d if d.get("day") == today else {"day": today, "paid": 0}

    def check(self, costly):
        with self.lock:
            now = time.time(); self.window = [t for t in self.window if now - t < 60]
            if len(self.window) >= self.per_min: return "rate limit: too many requests per minute"
            self.window.append(now)
            if not costly: return None
            d = self._load()
            if d["paid"] >= self.daily: return "daily cost cap reached for remote paid calls; try tomorrow or ask on the box"
            d["paid"] += 1
            self.path.parent.mkdir(parents=True, exist_ok=True)
            tmp = self.path.with_suffix(".tmp"); tmp.write_text(json.dumps(d)); os.replace(tmp, self.path)
            return None


class RemoteSprouts:
    """What remote callers get: Kiwi-repo-only reads, no secrets, no student data."""
    def __init__(self, sp, root=None):
        self.sp = sp; self.root = Path(root or KIWI).resolve()

    def docs_search(self, query, k=5):
        k = max(1, min(int(k), 20))
        r = subprocess.run(["rg", "-n", "-F", "-i", "-m", "3", "-g", "*.md", "--", str(query), "."],
                           cwd=self.root, capture_output=True, text=True, timeout=60)
        keep = []
        for l in r.stdout.splitlines():
            rel = l.split(":", 1)[0]
            p = (self.root / rel)
            if remote_path_denied(rel) or p.is_symlink() or not p.resolve().is_relative_to(self.root): continue
            keep.append(l)
        return scrub({"engine": "rg (remote, Kiwi *.md only)", "out": "\n".join(keep[: k * 3])})

    def kiwi_rule(self, id):
        if not RULE_ID.match(str(id)) or remote_path_denied(id): raise ValueError("denied")
        r = self.sp.kiwi_rule(id)
        if isinstance(r, dict) and remote_path_denied(r.get("path", "")): raise ValueError("denied")
        return scrub(r)

    def context_for(self, principal, task, **kw):
        kw.pop("paths", None)  # remote callers cannot steer file reads
        return scrub(self.sp.context_for("remote:" + str(principal)[:60], task, **kw))

    def rule_resources(self):
        return [r for r in self.sp.rule_resources() if not remote_path_denied(r["name"])]

    def read_resource(self, uri):
        if not uri.startswith("kiwi://rule/"): raise ValueError("unknown resource")
        r = self.kiwi_rule(uri[len("kiwi://rule/"):])
        return {"contents": [{"uri": uri, "mimeType": "text/markdown", "text": r["body"]}]}

    def __getattr__(self, name):
        if name not in HTTP_TOOLS: raise AttributeError(name)
        f = getattr(self.sp, name)

        def invoke(*args, **kwargs):
            if name in GUARDED_TOOLS:
                ident = kwargs.get("principal") or kwargs.get("agent_id") or kwargs.get("reporter")
                if ident and not str(ident).startswith("remote:"):
                    # Same identity context_for records for a remote caller.
                    kwargs["principal"] = "remote:" + str(ident)[:60]
            return scrub(f(*args, **kwargs))

        return invoke
DEFAULT_TOKEN_FILE = "~/.kiwi/secrets/kiwi-live-token"


def load_token(path=None):
    path = Path(os.path.expanduser(path or os.environ.get("KIWI_LIVE_TOKEN_FILE") or DEFAULT_TOKEN_FILE))
    if not path.is_file(): raise SystemExit(f"kiwi-live: token file missing: {path} (see decisions/2026-10-24-kiwi-live-access.md)")
    if os.name == "posix" and path.stat().st_mode & 0o077: raise SystemExit(f"kiwi-live: token file must be chmod 600: {path}")
    tok = path.read_text(encoding="utf-8").strip()
    if len(tok) < 32: raise SystemExit("kiwi-live: token too short (need >= 32 chars)")
    return tok


def http_handle(sp, msg, lock, budget=None):
    """Filter tool surface for remote callers, then delegate to handle()."""
    if not isinstance(msg, dict): return {"jsonrpc": "2.0", "id": None, "error": {"code": -32600, "message": "invalid request"}}
    err = lambda m: {"jsonrpc": "2.0", "id": msg.get("id"), "error": {"code": -32000, "message": m}} if msg.get("id") is not None else None
    if msg.get("method") == "tools/call":
        name = (msg.get("params") or {}).get("name")
        if name not in HTTP_TOOLS:
            return {"jsonrpc": "2.0", "id": msg.get("id"), "error": {"code": -32601, "message": "tool not available over remote access"}} if msg.get("id") is not None else None
        args = (msg.get("params") or {}).get("arguments") or {}
        costly = name in COSTLY and (name != "context_for" or os.environ.get("KIWI_CONTEXT_ANSWER", "").lower() != "off")
        if budget and (why := budget.check(costly)): return err(why)
    elif budget and (why := budget.check(False)): return err(why)
    with lock: out = handle(sp, msg)
    if isinstance(msg, dict) and msg.get("method") == "tools/list" and out and "result" in out:
        out["result"]["tools"] = [t for t in out["result"]["tools"] if t["name"] in HTTP_TOOLS]
    return out


def make_http_server(sp, token, host="127.0.0.1", port=8765, path="/mcp", budget=None, remote=True, events=None):
    import hmac
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
    events = events or getattr(sp, "events", None)
    if remote and not isinstance(sp, RemoteSprouts): sp = RemoteSprouts(sp)
    budget = budget or Budget()
    events = events or EventBus(getattr(sp, "root", KIWI))
    root = Path(getattr(sp, "root", KIWI))
    lock = threading.Lock(); want = ("Bearer " + token).encode()

    class H(BaseHTTPRequestHandler):
        server_version = "kiwi-sprouts"; sys_version = ""
        def log_message(self, fmt, *a):  # never log headers (token)
            sys.stderr.write("kiwi-live %s %s\n" % (self.address_string(), fmt % a))
        def _send(self, code, body=None, ctype="application/json"):
            data = b"" if body is None else json.dumps(body, ensure_ascii=False).encode()
            self.send_response(code)
            if body is not None: self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(data)))
            if code == 401: self.send_header("WWW-Authenticate", 'Bearer realm="kiwi"')
            self.end_headers(); self.wfile.write(data)
        def _authed(self):
            got = (self.headers.get("Authorization") or "").encode()
            return hmac.compare_digest(got, want)
        def _body(self):
            n = int(self.headers.get("Content-Length") or 0)
            if n <= 0 or n > 1_000_000: return None
            try: return json.loads(self.rfile.read(n))
            except Exception: return None
        def _webhook(self):
            """POST /webhooks: subscribe. GET /webhooks: list. DELETE /webhooks/<id>: remove."""
            if not self._authed(): return self._send(401, {"error": "unauthorized"})
            if self.command == "GET":
                return self._send(200, {"subscriptions": events.subscriptions()})
            if self.command == "DELETE":
                sid = self.path.split("?")[0].rsplit("/", 1)[-1]
                try: return self._send(200, events.unsubscribe(sid))
                except ValueError as e: return self._send(404, {"error": str(e)})
            body = self._body()
            if not isinstance(body, dict): return self._send(400, {"error": "JSON object body required"})
            try:
                sub = events.subscribe(body.get("url"), body.get("events"), body.get("description", ""))
            except ValueError as e:
                return self._send(400, {"error": str(e)})
            return self._send(201, {"subscription": sub,
                                    "signature": "X-Kiwi-Signature: v1=HMAC-SHA256(secret, timestamp + '.' + raw_body)"})
        def _host_load(self):
            """POST /host-load: store one dsh load report under ~/.kiwi/host-load/<id>.json."""
            if not self._authed(): return self._send(401, {"error": "unauthorized"})
            body = self._body()
            if not isinstance(body, dict): return self._send(400, {"error": "JSON object body required"})
            host = str(body.get("host") or "").strip()
            try:
                stored = kiwi_dsh_host.write_report(host, body)
            except ValueError as e:
                return self._send(400, {"error": str(e)})
            return self._send(200, {"host": host, "stored": str(stored)})
        def _event(self):
            """POST /events {"event", "data"}: local producers (kiwi oss) emit through the one bus. Only names in
            tokens webhook.external_events pass, so claim/token events stay owned by kiwi-live."""
            if not self._authed(): return self._send(401, {"error": "unauthorized"})
            body = self._body()
            if not isinstance(body, dict) or not isinstance(body.get("data"), dict):
                return self._send(400, {"error": "JSON object body with an object data required"})
            try: allowed = json.loads((root / "tokens" / "tokens.json").read_text(encoding="utf-8"))["webhook"]["external_events"]
            except (OSError, ValueError, KeyError, TypeError): allowed = []
            if body.get("event") not in allowed:
                return self._send(403, {"error": "event not in webhook.external_events"})
            if SECRET_LINE.search(json.dumps(body["data"], ensure_ascii=False)):
                return self._send(400, {"error": "refusing to emit a secret"})
            return self._send(202, events.emit(body["event"], body["data"]))
        def do_GET(self):
            route = self.path.split("?")[0]
            if route == "/healthz": return self._send(200, {"ok": True})
            if route in ("/webhooks", "/webhooks/"): return self._webhook()
            if not self._authed(): return self._send(401, {"error": "unauthorized"})
            self._send(405, {"error": "SSE stream not offered; POST JSON-RPC to " + path})
        def do_DELETE(self):
            if self.path.split("?")[0].startswith("/webhooks/"): return self._webhook()
            if not self._authed(): return self._send(401, {"error": "unauthorized"})
            self._send(405, {"error": "stateless server"})
        def do_POST(self):
            route = self.path.split("?")[0]
            if route in ("/webhooks", "/webhooks/subscribe"): return self._webhook()
            if route == "/host-load": return self._host_load()
            if route == "/events": return self._event()
            if route != path: return self._send(404, {"error": "not found"})
            if not self._authed(): return self._send(401, {"error": "unauthorized"})
            n = int(self.headers.get("Content-Length") or 0)
            if n <= 0 or n > 1_000_000: return self._send(413 if n else 400, {"error": "bad body size"})
            try: msg = json.loads(self.rfile.read(n))
            except Exception: return self._send(400, {"jsonrpc": "2.0", "id": None, "error": {"code": -32700, "message": "parse error"}})
            if isinstance(msg, list):
                outs = [o for o in (http_handle(sp, x, lock, budget) for x in msg) if o is not None]
                return self._send(200, outs) if outs else self._send(202)
            out = http_handle(sp, msg, lock, budget)
            return self._send(202) if out is None else self._send(200, out)

    return ThreadingHTTPServer((host, port), H)

def main(argv):
    root = None; host, port = "127.0.0.1", 8765
    it = iter(argv)
    for a in it:
        if a == "--root": root = next(it)
        elif a == "--host": host = next(it)
        elif a == "--port": port = int(next(it))
    token = load_token()
    events = EventBus()
    sp = Sprouts(root, events=events)
    srv = make_http_server(sp, token, host, port, events=events)
    print(f"kiwi-live: listening on http://{host}:{port}/mcp (bearer auth)", file=sys.stderr, flush=True)
    try: srv.serve_forever()
    finally: srv.server_close(); sp.close(); events.close()


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
