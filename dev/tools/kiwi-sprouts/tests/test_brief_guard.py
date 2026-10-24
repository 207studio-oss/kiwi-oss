import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import kiwi_brief
import kiwi_events
import kiwi_live
import kiwi_sprouts

REPO = Path(__file__).resolve().parents[4]
BOT = "00000000-0000-4000-8000-a3dedb5eb24a"


def make_repo():
    root = Path(tempfile.mkdtemp())
    (root / "bots" / "active").mkdir(parents=True)
    (root / "tokens").mkdir()
    (root / "bots" / "registry.toml").write_text(
        'version = 1\nupdated = "2026-10-24"\nsource = "test"\n\n'
        f'[[bot]]\nid = "{BOT}"\nname = "알파 (Alpha)"\npast_names = []\nrole = "test"\nowns = []\nnotes = ""\n',
        encoding="utf-8")
    tokens = json.loads((REPO / "tokens" / "tokens.json").read_text(encoding="utf-8"))
    (root / "tokens" / "tokens.json").write_text(json.dumps(tokens, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    for args in (("init", "-q"), ("config", "user.name", "kiwi"),
                 ("config", "user.email", "kiwi@example.com"), ("add", "-A"),
                 ("commit", "-q", "-m", "test: base")):
        subprocess.run(["git", "-C", str(root), *args], check=True, capture_output=True)
    return root


class BriefGuardTest(unittest.TestCase):
    def setUp(self):
        kiwi_sprouts.CONTEXT_CACHE.clear()
        self.root = make_repo()
        self.addCleanup(shutil.rmtree, self.root, ignore_errors=True)
        self.state = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.state, ignore_errors=True)
        self.ledger = self.state / "brief-ledger.jsonl"
        self.env = mock.patch.dict(os.environ, {"KIWI_BRIEF_LEDGER": str(self.ledger)}, clear=False)
        self.env.start()
        self.addCleanup(self.env.stop)
        self.sp = kiwi_sprouts.Sprouts(self.root, events=kiwi_events.EventBus(self.state))

    def set_mode(self, mode):
        path = self.root / "tokens" / "tokens.json"
        tokens = json.loads(path.read_text(encoding="utf-8"))
        tokens["brief_guard"]["mode"] = mode
        path.write_text(json.dumps(tokens, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    def ledger_records(self):
        return [json.loads(line) for line in self.ledger.read_text(encoding="utf-8").splitlines()]

    def context(self, task="wire the brief guard", principal=BOT):
        bundle = {"bundle_id": "fixture", "mandatory_rules": [{"id": "must"}]}
        answer = {"status": "answered", "answer": "ok", "confidence": 0.9}
        with mock.patch.object(self.sp, "_context_bundle", return_value=dict(bundle)), \
             mock.patch.object(self.sp, "ask", return_value=answer) as ask:
            result = self.sp.context_for(principal, task)
        return result, ask

    def test_missing_brief_warns_then_blocks(self):
        first = self.sp.claim_register("207studio-founder/kiwi", "brief warn", BOT, branch="bot/brief-warn")
        self.assertEqual(first["action"], "registered")
        self.assertEqual(first["brief_guard"]["status"], "missing")
        self.assertIn("context_for", first["brief_guard"]["message"])

        decision = self.sp.record_decision("use the brief ledger", BOT, commit=False)
        self.assertEqual(decision["brief_guard"]["status"], "missing")
        record = next(r for r in self.ledger_records()
                      if r.get("event") == "call" and r.get("tool") == "claim_register")
        self.assertEqual((record["principal"], record["brief"], record["action"]), (BOT, "missing", "ok"))

        self.set_mode("block")
        before = sorted((self.root / "bots" / "active").glob("*.toml"))
        with self.assertRaises(ValueError) as refused:
            self.sp.claim_register("207studio-founder/kiwi", "brief block", BOT, branch="bot/brief-block")
        self.assertIn("context_for", str(refused.exception))
        self.assertEqual(sorted((self.root / "bots" / "active").glob("*.toml")), before)
        with self.assertRaises(ValueError):
            self.sp.record_decision("blocked decision", BOT, commit=False)

    def test_unidentified_caller_is_flagged_in_block_mode(self):
        self.set_mode("block")
        with self.assertRaises(ValueError) as refused:
            self.sp.subscribe("https://example.test/hook")
        self.assertIn("unidentified", str(refused.exception))
        self.assertIn("context_for", str(refused.exception))

    def test_unidentified_caller_warning_in_warn_mode(self):
        result = self.sp.subscribe("https://example.test/hook")
        self.assertEqual(result["brief_guard"]["status"], "unidentified")
        self.assertIn("unidentified", result["brief_guard"]["message"])
        with mock.patch.object(self.sp, "_kiwi", return_value={"ok": True}):
            audit = self.sp.kiwi_status()["brief_audit"]
        self.assertEqual(audit["principals"][kiwi_brief.UNIDENTIFIED]["missing_calls_24h"], 1)

    def test_remote_caller_identity_matches_its_brief(self):
        remote = kiwi_live.RemoteSprouts(self.sp, self.root)
        with mock.patch.object(self.sp, "_context_bundle", return_value={"bundle_id": "x"}), \
             mock.patch.object(self.sp, "ask", return_value={"status": "answered", "answer": "ok", "confidence": 0.9}):
            remote.context_for(BOT, "wire the brief guard")
        result = remote.record_decision(line="remote decision", reporter=BOT, commit=False)
        self.assertNotIn("brief_guard", result)
        record = next(r for r in self.ledger_records()
                      if r.get("event") == "call" and r.get("tool") == "record_decision")
        self.assertEqual(record["principal"], "remote:" + BOT)

    def test_brief_clears_the_warning(self):
        brief, _ = self.context()
        self.assertNotIn("brief_guard", brief)
        decision = self.sp.record_decision("briefed decision", BOT, commit=False)
        self.assertNotIn("brief_guard", decision)

    def test_status_audit_counts_missing_calls_and_last_brief(self):
        self.sp.record_decision("one", BOT, commit=False)
        self.sp.record_decision("two", BOT, commit=False)
        with mock.patch.object(self.sp, "_kiwi", return_value={"ok": True}):
            audit = self.sp.kiwi_status()["brief_audit"]
        row = audit["principals"][BOT]
        self.assertEqual(row["missing_calls_24h"], 2)
        self.assertIsNone(row["last_brief"])

        self.context()
        with mock.patch.object(self.sp, "_kiwi", return_value={"ok": True}):
            audit = self.sp.kiwi_status()["brief_audit"]
        self.assertIsNotNone(audit["principals"][BOT]["last_brief"])
        self.assertEqual(audit["principals"][BOT]["missing_calls_24h"], 2)  # still in the last 24h

    def test_status_reports_audit_when_core_is_unavailable(self):
        self.sp.record_decision("one", BOT, commit=False)
        with mock.patch.object(self.sp, "_kiwi", side_effect=FileNotFoundError("kiwi")):
            status = self.sp.kiwi_status()
        self.assertIn("status_error", status)
        self.assertEqual(status["brief_audit"]["principals"][BOT]["missing_calls_24h"], 1)

    def test_task_guard_is_recorded_with_principal_and_task(self):
        with mock.patch.object(self.sp, "_kiwi", return_value={"verdict": "ok"}):
            self.sp.task_guard("task-1", 7, principal=BOT)
        record = next(r for r in self.ledger_records() if r.get("event") == "guard")
        self.assertEqual((record["principal"], record["task"]), (BOT, "task-1"))

    def test_routing_task_passes_but_risky_task_does_not(self):
        for task in ("relay this message to the next bot", "메시지를 다음 봇에게 전달해줘",
                     "routing: pick the owner", "forwarding this note"):
            with self.subTest(task=task):
                result, ask = self.context(task)
                self.assertEqual(result["status"], "pass")
                self.assertIsNone(result["needs_user"])
                self.assertGreaterEqual(result["confidence"], 0.8)
                self.assertIn("저위험", result["answer"])
                ask.assert_not_called()

        config = self.sp.brief.config()
        self.assertIsNone(kiwi_brief.calibrated_pass("fix the router crash", config))
        self.assertIsNone(kiwi_brief.calibrated_pass("forward the payment", config))

        risky = {"status": "needs_user", "answer": "오너 확인 필요", "confidence": 0.2,
                 "reason": "low_confidence", "best_guess": "ask first"}
        for task in ("forward the payment confirmation to 외부", "relay the secret token"):
            with self.subTest(task=task):
                with mock.patch.object(self.sp, "_context_bundle", return_value={"bundle_id": "x"}), \
                     mock.patch.object(self.sp, "ask", return_value=risky) as ask:
                    result = self.sp.context_for(BOT, task)
                ask.assert_called_once()
                self.assertNotEqual(result.get("status"), "pass")
                self.assertEqual(result["needs_user"]["reason"], "low_confidence")

    def test_config_lives_in_tokens(self):
        config = kiwi_brief.BriefGuard(self.root).config()
        self.assertIn("relay", config["routing_keywords"])
        self.assertIn("payment", config["risky_keywords"])
        self.assertEqual((config["window_min"], config["mode"]), (60.0, "warn"))


if __name__ == "__main__":
    unittest.main()
