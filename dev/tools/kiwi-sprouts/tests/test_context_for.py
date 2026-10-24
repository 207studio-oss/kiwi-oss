import json, os, subprocess, sys, tempfile, unittest
from pathlib import Path
sys.path[:0] = [str(Path(__file__).resolve().parent), str(Path(__file__).resolve().parent.parent)]
import sprouts_support as ks
import kiwi_sprouts as python_sprouts
from unittest import mock

KIWI = Path(os.environ.get("KIWI_TEST_CORE_BIN", Path(__file__).resolve().parents[2] / ("kiwi-core/target/debug/kiwi.exe" if os.name == "nt" else "kiwi-core/target/debug/kiwi")))


class ContextAnswer(unittest.TestCase):
    def setUp(self):
        python_sprouts.CONTEXT_CACHE.clear()
        self.env = mock.patch.dict(os.environ, {"KIWI_BRIEF_LEDGER": str(Path(tempfile.mkdtemp()) / "brief-ledger.jsonl")}, clear=False)
        self.env.start()
        self.addCleanup(self.env.stop)

    def test_question_defaults_to_task_and_ask_receives_task_and_principal(self):
        sp = python_sprouts.Sprouts()
        bundle = {"bundle_id": "fixture", "mandatory_rules": [{"id": "must"}]}
        answered = {"status": "answered", "answer": "bot", "confidence": 0.9}
        with mock.patch.object(sp, "_context_bundle", side_effect=lambda *args: dict(bundle)), mock.patch.object(sp, "ask", return_value=answered) as ask:
            result = sp.context_for("bot", "task")
            ask.assert_called_once_with("task", None, task="task", principal="bot")
            self.assertEqual(result, {"answer": "bot", "confidence": 0.9, "evidence": [], "needs_user": None,
                                      "ask_id": None, **bundle, "answer_detail": answered})
            result = sp.context_for("bot", "task", question="", options=["bot", "user"])
            ask.assert_called_with("", ["bot", "user"], task="task", principal="bot")
            self.assertEqual(result["answer"], "bot")
            self.assertEqual(result["bundle_id"], "fixture")

    def test_ask_failures_preserve_context(self):
        sp = python_sprouts.Sprouts()
        bundle = {"bundle_id": "fixture", "mandatory_rules": [{"id": "must"}]}
        for error in (ValueError("failed"), SystemExit("stop"), subprocess.TimeoutExpired("kiwi-ask", 90),
                      ModuleNotFoundError("No module named 'ask'"), FileNotFoundError("kiwi-ask missing")):
            with self.subTest(error=type(error).__name__), mock.patch.object(sp, "_context_bundle", return_value=dict(bundle)), mock.patch.object(sp, "ask", side_effect=error):
                result = sp.context_for("bot", "task", question="q")
                self.assertEqual(result, {"answer": None, "confidence": 0.0, "evidence": [], "ask_id": None,
                                          "needs_user": {"reason": "decisions_unavailable"},
                                          **bundle, "answer_detail": None})


@unittest.skipUnless(KIWI.is_file(), "kiwi-core not built")
class ContextFor(unittest.TestCase):
    def setUp(self):
        self.home = tempfile.mkdtemp(); src = Path(tempfile.mkdtemp())
        (src / "rules").mkdir(); (src / "accounts").mkdir()
        (src / "rules/must.md").write_text("---\nmandatory: true\n---\n# Must\nalways", newline="\n")
        (src / "rules/deploy.md").write_text("# Deploy\nhow to deploy")
        (src / "accounts/k.md").write_text("sk-NOPE")
        self.env = {"KIWI_HOME": self.home, "KIWI_DB": self.home + "/kiwi.db", "KIWI_BIN": str(KIWI), "KIWI_DECISIONS": "off", "KIWI_AUTOREFRESH": "off", "KIWI_SEMANTIC": "off",
                    "KIWI_BRIEF_LEDGER": self.home + "/brief-ledger.jsonl"}
        subprocess.run([str(KIWI), "compile", "--src", str(src), "--out", self.home + "/kiwi.db", "--sha", "s"], check=True, capture_output=True, env={**os.environ, **self.env})

    def test_tool_listed_and_bundle_returned(self):
        self.assertIn("context_for", ks.TOOLS)
        from unittest import mock
        with mock.patch.dict(os.environ, self.env):
            b = ks.call(ks.Sprouts(self.home), "context_for", {"principal": "bot", "task": "deploy app"})
            self.assertEqual(b["mandatory_rules"][0]["id"], "must")
            self.assertEqual(b["selected_context"][0]["id"], "deploy")
            self.assertNotIn("sk-NOPE", json.dumps(b))
            self.assertEqual(ks.call(ks.Sprouts(self.home), "kiwi_rule", {"id": "deploy"})["id"], "deploy")
            with self.assertRaises(Exception): ks.Sprouts(self.home).kiwi_rule("accounts/k")  # not a rule node (compiler scope), not a block

    def test_mcp_resources(self):
        import io
        from unittest import mock
        msgs = [{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}, {"jsonrpc": "2.0", "id": 2, "method": "resources/list"},
                {"jsonrpc": "2.0", "id": 3, "method": "resources/read", "params": {"uri": "kiwi://rule/deploy"}},
                {"jsonrpc": "2.0", "id": 4, "method": "resources/read", "params": {"uri": "kiwi://rule/accounts/k"}}]
        out = io.StringIO()
        with mock.patch.dict(os.environ, self.env), mock.patch.object(sys, "stdin", io.StringIO("\n".join(json.dumps(m) for m in msgs))), mock.patch.object(sys, "stdout", out):
            ks.mcp(ks.Sprouts(self.home))
        r = [json.loads(l) for l in out.getvalue().splitlines()]
        self.assertIn("resources", r[0]["result"]["capabilities"]); self.assertIn("context_for", r[0]["result"]["instructions"])
        self.assertIn("result", r[1], r[1]); uris = [x["uri"] for x in r[1]["result"]["resources"]]
        self.assertIn("kiwi://rule/deploy", uris); self.assertFalse(any("accounts" in u for u in uris))
        self.assertIn("how to deploy", r[2]["result"]["contents"][0]["text"])
        self.assertIn("error", r[3])


if __name__ == "__main__":
    unittest.main()
