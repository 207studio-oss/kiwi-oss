import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import kiwi_sprouts

BUNDLE = {"bundle_id": "fixture", "mandatory_rules": [{"id": "must"}]}


class ContextAnswerTest(unittest.TestCase):
    def setUp(self):
        kiwi_sprouts.CONTEXT_CACHE.clear()
        self.env = mock.patch.dict(os.environ, {"KIWI_BRIEF_LEDGER": str(Path(tempfile.mkdtemp()) / "brief-ledger.jsonl")}, clear=False)
        self.env.start()
        self.addCleanup(self.env.stop)
        os.environ.pop("KIWI_CONTEXT_ANSWER", None)

    def sp(self, ask):
        sp = kiwi_sprouts.Sprouts()
        bundle = mock.patch.object(sp, "_context_bundle", side_effect=lambda *args: dict(BUNDLE))
        bundle.start()
        self.addCleanup(bundle.stop)
        ask_mock = mock.patch.object(sp, "ask", side_effect=ask) if isinstance(ask, Exception) else mock.patch.object(sp, "ask", return_value=ask)
        ask_mock.start()
        self.addCleanup(ask_mock.stop)
        return sp, sp.ask

    def test_answered(self):
        answer = {"ask_id": "ask-1", "status": "answered", "answer": "Use the lock", "confidence": 0.9,
                  "evidence": [{"id": "rules/lock"}, {"id": "rules/write"}], "decisions": [{"id": "dec-1"}]}
        sp, ask = self.sp(answer)
        result = sp.context_for("bot", "task", question="q", options=["a", "b"])
        ask.assert_called_once_with("q", ["a", "b"], task="task", principal="bot")
        self.assertEqual(list(result)[:5], ["answer", "confidence", "evidence", "needs_user", "ask_id"])
        self.assertEqual(result["answer"], "Use the lock")
        self.assertEqual(result["confidence"], 0.9)
        self.assertEqual(result["evidence"], ["rules/lock", "rules/write", "dec-1"])
        self.assertIsNone(result["needs_user"])
        self.assertEqual(result["ask_id"], "ask-1")
        self.assertEqual(result["bundle_id"], "fixture")
        self.assertEqual(result["mandatory_rules"], [{"id": "must"}])
        self.assertEqual(result["answer_detail"], answer)

    def test_risky(self):
        answer = {"ask_id": "ask-2", "status": "needs_user", "answer": "오너 확인 필요",
                  "confidence": 0.2, "reason": "risk: deploy", "best_guess": "ask first",
                  "evidence": [{"id": "rules/deploy"}]}
        sp, _ = self.sp(answer)
        result = sp.context_for("bot", "task")
        self.assertIsNone(result["answer"])
        self.assertEqual(result["confidence"], 0.2)
        self.assertEqual(result["evidence"], ["rules/deploy"])
        self.assertEqual(result["needs_user"], {"reason": "risk: deploy", "best_guess": "ask first"})
        self.assertEqual(result["answer_detail"], answer)

    def test_ask_failure_keeps_bundle(self):
        sp, _ = self.sp(ValueError("boom"))
        result = sp.context_for("bot", "task", question="q")
        self.assertIsNone(result["answer"])
        self.assertEqual(result["confidence"], 0.0)
        self.assertEqual(result["evidence"], [])
        self.assertEqual(result["needs_user"], {"reason": "decisions_unavailable"})
        self.assertIsNone(result["ask_id"])
        self.assertIsNone(result["answer_detail"])
        for key, value in BUNDLE.items():
            self.assertEqual(result[key], value)

    def test_cache_hit_calls_ask_once(self):
        answer = {"ask_id": "ask-3", "status": "answered", "answer": "Cached", "confidence": 0.8}
        sp, ask = self.sp(answer)
        first = sp.context_for("bot", "task", question="q", options=["a"])
        second = sp.context_for("bot", "task", question="q", options=["a"])
        ask.assert_called_once()
        self.assertEqual(first, second)
        self.assertEqual(second["answer"], "Cached")

    def test_disabled_does_not_ask(self):
        sp, ask = self.sp({"status": "answered", "answer": "unused", "confidence": 1.0})
        with mock.patch.dict(os.environ, {"KIWI_CONTEXT_ANSWER": "off"}):
            result = sp.context_for("bot", "task", question="q")
        ask.assert_not_called()
        self.assertIsNone(result["answer"])
        self.assertEqual(result["needs_user"], {"reason": "disabled"})


if __name__ == "__main__":
    unittest.main()
