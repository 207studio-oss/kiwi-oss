import json, os, sys, tempfile, threading, unittest, urllib.request, urllib.error
from pathlib import Path
from unittest import mock
HERE = Path(__file__).resolve().parent
sys.path[:0] = [str(HERE.parent)]
import kiwi_live as ks

TOKEN = "t" * 40


def post(url, body, auth="Bearer " + TOKEN):
    h = {"Content-Type": "application/json"}
    if auth: h["Authorization"] = auth
    req = urllib.request.Request(url, json.dumps(body).encode(), h)
    try:
        with urllib.request.urlopen(req) as r: return r.status, (json.loads(r.read() or b"null"))
    except urllib.error.HTTPError as e: return e.code, None


class FakeSprouts:
    def kiwi_status(self): return {"ok": True}
    def dsh_host(self, mac_only=False): return {"host": "mac-mini" if mac_only else "box", "mac_only": mac_only}
    def code_replace_body(self, **a): raise AssertionError("write tool reached over HTTP")
    def rule_resources(self): return []


class Http(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.srv = ks.make_http_server(FakeSprouts(), TOKEN, "127.0.0.1", 0)
        cls.url = "http://127.0.0.1:%d/mcp" % cls.srv.server_address[1]
        threading.Thread(target=cls.srv.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls): cls.srv.shutdown(); cls.srv.server_close()

    def post(self, body, auth="Bearer " + TOKEN): return post(self.url, body, auth)

    def post_url(self, url, body, auth="Bearer " + TOKEN): return post(url, body, auth)

    def test_requires_token(self):
        self.assertEqual(self.post({"jsonrpc": "2.0", "id": 1, "method": "ping"}, auth=None)[0], 401)
        self.assertEqual(self.post({"jsonrpc": "2.0", "id": 1, "method": "ping"}, auth="Bearer wrong")[0], 401)

    def test_host_load_requires_token_and_stores_report(self):
        url = "http://127.0.0.1:%d/host-load" % self.srv.server_address[1]
        body = {"host": "windows-studio-pc", "cpu_percent": 12, "memory_percent": 34, "running_dsh": 1}
        self.assertEqual(self.post_url(url, body, auth=None)[0], 401)
        with tempfile.TemporaryDirectory() as home, mock.patch.dict(os.environ, {"KIWI_HOME": home}):
            code, stored = self.post_url(url, body)
            self.assertEqual(code, 200, stored)
            path = Path(home) / "host-load" / "windows-studio-pc.json"
            self.assertTrue(path.is_file())
            self.assertEqual(json.loads(path.read_text())["host"], "windows-studio-pc")
            self.assertEqual(self.post_url(url, {**body, "host": "../escape"})[0], 400)

    def test_initialize_and_call(self):
        code, r = self.post({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})
        self.assertEqual(code, 200); self.assertEqual(r["result"]["serverInfo"]["name"], "kiwi-sprouts")
        code, r = self.post({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "kiwi_status", "arguments": {}}})
        self.assertEqual(json.loads(r["result"]["content"][0]["text"]), {"ok": True})

    def test_write_tools_hidden(self):
        _, r = self.post({"jsonrpc": "2.0", "id": 3, "method": "tools/list"})
        names = {t["name"] for t in r["result"]["tools"]}
        self.assertIn("ask", names); self.assertIn("dsh_host", names); self.assertNotIn("code_replace_body", names)
        _, r = self.post({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "code_replace_body", "arguments": {}}})
        self.assertIn("error", r)

    def test_dsh_host_tool_is_remote_callable(self):
        _, r = self.post({"jsonrpc": "2.0", "id": 8, "method": "tools/call",
                          "params": {"name": "dsh_host", "arguments": {"mac_only": True}}})
        self.assertEqual(json.loads(r["result"]["content"][0]["text"]), {"host": "mac-mini", "mac_only": True})

    def test_notification_202(self):
        self.assertEqual(self.post({"jsonrpc": "2.0", "method": "notifications/initialized"})[0], 202)

    def test_token_file_checks(self):
        d = tempfile.mkdtemp(); p = Path(d) / "tok"
        p.write_text("short"); os.chmod(p, 0o600)
        with self.assertRaises(SystemExit): ks.load_token(str(p))
        p.write_text(TOKEN); os.chmod(p, 0o644)
        with self.assertRaises(SystemExit): ks.load_token(str(p))
        os.chmod(p, 0o600); self.assertEqual(ks.load_token(str(p)), TOKEN)


class FakeBus:
    def __init__(self): self.emitted = []
    def emit(self, event, data): self.emitted.append((event, data)); return {"id": "evt-1", "event": event}


class Events(unittest.TestCase):
    """POST /events: local producers (kiwi oss) emit only tokens webhook.external_events through the bus."""
    @classmethod
    def setUpClass(cls):
        cls.root = Path(tempfile.mkdtemp()); (cls.root / "tokens").mkdir()
        (cls.root / "tokens" / "tokens.json").write_text(json.dumps({"webhook": {"external_events": ["oss.released"]}}), encoding="utf-8")
        sp = FakeSprouts(); sp.root = cls.root; cls.bus = FakeBus()
        cls.srv = ks.make_http_server(sp, TOKEN, "127.0.0.1", 0, remote=False, events=cls.bus)
        cls.url = "http://127.0.0.1:%d/events" % cls.srv.server_address[1]
        threading.Thread(target=cls.srv.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls): cls.srv.shutdown(); cls.srv.server_close()

    def test_auth_allowlist_and_secret_guard(self):
        ok = {"event": "oss.released", "data": {"tag": "v0.1.0"}}
        self.assertEqual(post(self.url, ok, auth=None)[0], 401)
        self.assertEqual(post(self.url, ok, auth="Bearer wrong")[0], 401)
        self.assertEqual(post(self.url, {"event": "token.changed", "data": {}})[0], 403)
        self.assertEqual(post(self.url, {"event": "oss.released", "data": "x"})[0], 400)
        self.assertEqual(post(self.url, {"event": "oss.released", "data": {"note": "sk-" + "0" * 20}})[0], 400)
        self.assertEqual(self.bus.emitted, [])
        self.assertEqual(post(self.url, ok), (202, {"id": "evt-1", "event": "oss.released"}))
        self.assertEqual(self.bus.emitted, [("oss.released", {"tag": "v0.1.0"})])


if __name__ == "__main__": unittest.main()


class RemoteSafety(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp())
        (self.root / "rules").mkdir(); (self.root / "accounts").mkdir(); (self.root / "students").mkdir()
        (self.root / "rules" / "a.md").write_text("kiwi needle ok\napi_key = sk-abcdefghijklmnopqrstuv\n")
        (self.root / "accounts" / "x.md").write_text("kiwi needle secret\n")
        (self.root / "students" / "s.md").write_text("kiwi needle 학생\n")
        outside = Path(tempfile.mkdtemp()) / "o.md"; outside.write_text("kiwi needle outside\n")
        (self.root / "rules" / "link.md").symlink_to(outside)
        class Inner:
            calls = []
            def kiwi_rule(s, id): s.calls.append(id); return {"path": "rules/" + id + ".md", "body": "token: abcdefghij\nfine"}
            def context_for(s, principal, task, **kw): s.calls.append(kw); return {"rules": ["password=hunter2hunter2", "keep"]}
            def rule_resources(s): return [{"name": "accounts/x"}, {"name": "rules/a"}]
        self.inner = Inner(); self.r = ks.RemoteSprouts(self.inner, self.root)

    def test_search_stays_in_repo_and_skips_secrets_students_symlinks(self):
        out = self.r.docs_search("needle")["out"]
        self.assertIn("rules/a.md", out)
        for bad in ("accounts", "students", "outside", "sk-abc"): self.assertNotIn(bad, out)

    def test_rule_id_traversal_and_denied_dirs(self):
        for bad in ("../etc/passwd", "/etc/passwd", "accounts/x", "rules/../../x", "secrets/k", "hwp-golden/a", ".env"):
            with self.assertRaises(ValueError, msg=bad): self.r.kiwi_rule(bad)
        self.assertEqual(self.r.kiwi_rule("ok")["body"], "[redacted]\nfine")

    def test_context_for_scrubs_and_drops_paths(self):
        out = self.r.context_for("codex", "t", paths=["/etc"])
        self.assertEqual(out["rules"], ["[redacted]", "keep"])
        self.assertNotIn("paths", self.inner.calls[-1])

    def test_resources_filtered(self):
        self.assertEqual([x["name"] for x in self.r.rule_resources()], ["rules/a"])

    def test_scrub_keeps_ask_ids_and_redacts_sk_secrets(self):
        ask_id = "ask-0123456789abcdef"
        self.assertEqual(ks.scrub({"ask_id": ask_id}), {"ask_id": ask_id})
        self.assertEqual(ks.scrub("ask_id: " + ask_id), "ask_id: " + ask_id)
        self.assertEqual(ks.scrub("sk-0123456789abcdef"), "[redacted]")
        self.assertEqual(ks.scrub("key sk-0123456789abcdef"), "[redacted]")


class Limits(unittest.TestCase):
    def test_daily_cap_and_rate(self):
        b = ks.Budget(path=str(Path(tempfile.mkdtemp()) / "u.json"), per_min=100, daily=2)
        self.assertIsNone(b.check(True)); self.assertIsNone(b.check(True))
        self.assertIn("daily", b.check(True)); self.assertIsNone(b.check(False))
        b2 = ks.Budget(path=b.path, per_min=100, daily=2)  # persisted across restart
        self.assertIn("daily", b2.check(True))
        r = ks.Budget(path=str(Path(tempfile.mkdtemp()) / "u.json"), per_min=2, daily=9)
        r.check(False); r.check(False); self.assertIn("rate", r.check(False))
