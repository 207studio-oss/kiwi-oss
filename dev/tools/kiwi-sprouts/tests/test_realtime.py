import json
import os
import queue
import subprocess
import sys
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path[:0] = [str(HERE.parent)]
import kiwi_events as ke
import kiwi_live as ks
from kiwi_sprouts import Sprouts

TOKEN = "t" * 40
BOT = "00000000-0000-4000-8000-a3dedb5eb24a"
OTHER = "00000000-0000-4000-8000-eee37f303215"


def receiver():
    inbox = queue.Queue()

    class H(BaseHTTPRequestHandler):
        def do_POST(self):
            n = int(self.headers.get("Content-Length") or 0)
            inbox.put({"headers": {k.lower(): v for k, v in self.headers.items()},
                       "body": self.rfile.read(n)})
            self.send_response(204)
            self.end_headers()

        def log_message(self, *a):
            pass

    srv = ThreadingHTTPServer(("127.0.0.1", 0), H)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv, inbox


def git(root, *args):
    return subprocess.run(["git", "-C", str(root), *args], capture_output=True, text=True)


def make_repo():
    root = Path(tempfile.mkdtemp())
    (root / "bots" / "active").mkdir(parents=True)
    (root / "tokens").mkdir()
    (root / "bots" / "registry.toml").write_text(
        'version = 1\nupdated = "2026-10-24"\nsource = "test"\n\n'
        f'[[bot]]\nid = "{BOT}"\nname = "알파 (Alpha)"\npast_names = []\nrole = "sample-app"\nowns = []\nnotes = ""\n\n'
        f'[[bot]]\nid = "{OTHER}"\nname = "베타"\npast_names = []\nrole = "test"\nowns = []\nnotes = ""\n',
        encoding="utf-8")
    (root / "tokens" / "tokens.json").write_text(json.dumps({
        "version": 1,
        "color": {"text": {"primary": "#111111"}},
        "motion": {"button_spring": {"web": {"duration_ms": 420, "easing": "cubic-bezier(0.34, 1.56, 0.64, 1)"}}},
        "claim": {"default_status": "active", "states": ["active", "expired"], "default_hours": 4},
    }, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    git(root, "init", "-q")
    git(root, "config", "user.name", "kiwi")
    git(root, "config", "user.email", "kiwi@example.com")
    git(root, "add", "-A")
    result = git(root, "commit", "-q", "-m", "test: base")
    if result.returncode:
        raise RuntimeError(result.stderr)
    return root


class Realtime(unittest.TestCase):
    def setUp(self):
        self.root = make_repo()
        self.state = Path(tempfile.mkdtemp())
        self.env = mock.patch.dict(os.environ, {"KIWI_BRIEF_LEDGER": str(self.state / "brief-ledger.jsonl")}, clear=False)
        self.env.start()
        self.addCleanup(self.env.stop)
        self.bus = ke.EventBus(self.state)
        self.recv, self.inbox = receiver()
        self.url = "http://127.0.0.1:%d/hook" % self.recv.server_address[1]
        self.http = ks.make_http_server(Sprouts(self.root, events=self.bus), TOKEN, "127.0.0.1", 0,
                                        events=self.bus, remote=False)
        self.mcp = "http://127.0.0.1:%d/mcp" % self.http.server_address[1]
        threading.Thread(target=self.http.serve_forever, daemon=True).start()

    def tearDown(self):
        self.http.shutdown()
        self.http.server_close()
        self.recv.shutdown()
        self.recv.server_close()
        self.bus.close()

    def post(self, url, body):
        req = urllib.request.Request(url, json.dumps(body).encode(),
                                     {"Content-Type": "application/json", "Authorization": "Bearer " + TOKEN})
        try:
            with urllib.request.urlopen(req) as r:
                return r.status, json.loads(r.read() or b"null")
        except urllib.error.HTTPError as e:
            return e.code, None

    def call(self, name, args, id=1):
        code, out = self.post(self.mcp, {"jsonrpc": "2.0", "id": id, "method": "tools/call",
                                         "params": {"name": name, "arguments": args}})
        self.assertEqual(code, 200, out)
        text = out["result"]["content"][0]["text"]
        self.assertFalse(out["result"].get("isError"), text)
        return json.loads(text)

    def event(self, bus=None):
        bus = bus or self.bus
        self.assertTrue(bus.wait_idle(6), "webhook delivery did not finish")
        return self.inbox.get(timeout=2)

    def test_subscribe_tool_then_claim_register_renews_releases_with_hmac(self):
        sub = self.call("subscribe", {"url": self.url, "events": ["*"], "description": "local e2e"})
        self.assertTrue(sub["id"].startswith("sub-"))
        secret = sub["secret"]

        reg = self.call("claim_register", {"repo": "207studio-founder/sample-app",
                                           "task": "button spring animation", "agent_id": BOT,
                                           "bot": "알파 (Alpha)", "branch": "alpha/button-spring"})
        self.assertEqual(reg["action"], "registered")
        self.assertTrue(reg["commit"])
        path = self.root / reg["path"]
        self.assertTrue(path.is_file())
        self.assertIn("button spring animation", path.read_text(encoding="utf-8"))
        self.assertEqual(git(self.root, "log", "-1", "--format=%s").stdout.strip(),
                         "claim: button spring animation (알파 (Alpha))")

        msg = self.event()
        self.assertEqual(msg["headers"]["x-kiwi-event"], "claim.registered")
        self.assertEqual(msg["headers"]["x-kiwi-delivery"], reg["event"]["id"])
        self.assertTrue(ks.EventBus.verify(secret, msg["headers"]["x-kiwi-timestamp"],
                                           msg["body"], msg["headers"]["x-kiwi-signature"]))
        envelope = json.loads(msg["body"])
        self.assertEqual(envelope["event"], "claim.registered")
        self.assertEqual(envelope["data"]["claim"]["repo"], "207studio-founder/sample-app")

        renewed = self.call("claim_renew", {"claim": reg["path"], "agent_id": BOT, "hours": 8})
        self.assertEqual(renewed["action"], "renewed")
        msg = self.event()
        self.assertEqual(msg["headers"]["x-kiwi-event"], "claim.renewed")
        self.assertTrue(ks.EventBus.verify(secret, msg["headers"]["x-kiwi-timestamp"],
                                           msg["body"], msg["headers"]["x-kiwi-signature"]))

        released = self.call("claim_release", {"claim": reg["path"], "agent_id": BOT})
        self.assertEqual(released["action"], "released")
        self.assertFalse(path.exists())
        msg = self.event()
        self.assertEqual(msg["headers"]["x-kiwi-event"], "claim.released")

    def test_description_only_claim_has_unknown_branch_and_no_conflict(self):
        one = self.call("claim_register", {"repo": "207studio-founder/sample-app",
                                           "task": "button spring animation", "agent_id": BOT})
        self.assertIn("branch_unknown = true", (self.root / one["path"]).read_text(encoding="utf-8"))
        two = self.call("claim_register", {"repo": "207studio-founder/sample-app",
                                           "task": "rewrite follow-up", "agent_id": BOT})
        self.assertEqual(two["action"], "registered")
        self.assertTrue((self.root / two["path"]).is_file())

    def test_conflict_unregistered_and_idempotent(self):
        reg = self.call("claim_register", {"repo": "207studio-founder/sample-app",
                                           "task": "button spring animation", "agent_id": BOT,
                                           "branch": "alpha/button-spring", "paths": ["src/buttons/"]})
        again = self.call("claim_register", {"repo": "207studio-founder/sample-app",
                                             "task": "button spring animation", "agent_id": BOT,
                                             "branch": "alpha/button-spring", "paths": ["src/buttons/"]})
        self.assertEqual(again["action"], "unchanged")
        self.assertIsNone(again["event"])
        self.assertTrue(self.inbox.empty())
        with self.assertRaises(ValueError):
            ke.Claims(self.root, self.bus).register("207studio-founder/sample-app", "x",
                                                    "00000000-0000-4000-8000-12b9377cbe7e")
        with self.assertRaises(ValueError):
            ke.Claims(self.root, self.bus).register("207studio-founder/sample-app", "other",
                                                    OTHER, branch="alpha/button-spring",
                                                    paths=["src/buttons/new/"])
        self.assertEqual(reg["action"], "registered")

    def test_token_set_emits_token_changed(self):
        sub = self.call("subscribe", {"url": self.url})
        tok = self.call("token_set", {"path": "motion.button_spring.web.duration_ms",
                                      "value": 460, "reporter": BOT})
        self.assertEqual(tok["action"], "changed")
        self.assertEqual(tok["old"], 420)
        self.assertEqual(tok["new"], 460)
        saved = json.loads((self.root / "tokens" / "tokens.json").read_text(encoding="utf-8"))
        self.assertEqual(saved["motion"]["button_spring"]["web"]["duration_ms"], 460)
        msg = self.event()
        self.assertEqual(msg["headers"]["x-kiwi-event"], "token.changed")
        self.assertTrue(ks.EventBus.verify(sub["secret"], msg["headers"]["x-kiwi-timestamp"],
                                           msg["body"], msg["headers"]["x-kiwi-signature"]))
        data = json.loads(msg["body"])["data"]
        self.assertEqual(data["path"], "motion.button_spring.web.duration_ms")
        self.assertEqual(data["new"], 460)

    def test_token_set_create_new_key(self):
        with self.assertRaises(Exception):
            self.call("token_set", {"path": "color.new_group.fill", "value": "#fff", "reporter": BOT})
        tok = self.call("token_set", {"path": "color.new_group.fill", "value": "#fff",
                                      "reporter": BOT, "create": True})
        self.assertEqual(tok["action"], "changed")
        self.assertIsNone(tok["old"])
        saved = json.loads((self.root / "tokens" / "tokens.json").read_text(encoding="utf-8"))
        self.assertEqual(saved["color"]["new_group"]["fill"], "#fff")

    def test_http_webhook_endpoint_and_listing(self):
        code, out = self.post("http://127.0.0.1:%d/webhooks" % self.http.server_address[1],
                              {"url": self.url, "events": ["claim.registered"]})
        self.assertEqual(code, 201)
        sub = out["subscription"]
        self.assertTrue(sub["secret"])
        code, out = self.post(self.mcp, {"jsonrpc": "2.0", "id": 9, "method": "tools/call",
                                         "params": {"name": "subscriptions", "arguments": {}}})
        listed = json.loads(out["result"]["content"][0]["text"])
        self.assertEqual([s["id"] for s in listed], [sub["id"]])
        self.assertNotIn("secret", listed[0])
        self.bus.emit("claim.registered", {"path": "x", "commit": "y", "claim": {"repo": "a/b"}})
        msg = self.event()
        self.assertEqual(msg["headers"]["x-kiwi-event"], "claim.registered")
        self.assertTrue(ks.EventBus.verify(sub["secret"], msg["headers"]["x-kiwi-timestamp"],
                                           msg["body"], msg["headers"]["x-kiwi-signature"]))


if __name__ == "__main__":
    unittest.main()
