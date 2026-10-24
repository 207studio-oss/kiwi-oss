import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path[:0] = [str(HERE), str(HERE.parent)]
import kiwi_brief
import kiwi_lessons
import kiwi_live
import kiwi_selfimprove as si
import kiwi_sprouts
from kiwi_events import EventBus


class Bus:
    def __init__(self):
        self.events = []

    def emit(self, event, data):
        self.events.append((event, data))
        return {"event": event, "id": "evt-test"}

    def subscriptions(self):
        return []


class TempRepo(unittest.TestCase):
    def tokens_config(self):
        return {}

    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="kiwi-selfimprove-"))
        self.addCleanup(shutil.rmtree, self.root, ignore_errors=True)
        (self.root / "tokens").mkdir()
        (self.root / "rules").mkdir()
        (self.root / "lessons").mkdir()
        (self.root / "lessons" / "README.md").write_text("# lessons\n", encoding="utf-8")
        (self.root / "rules" / "code-review.md").write_text("# code review\n", encoding="utf-8")
        (self.root / "rules" / "agent-procedure.md").write_text("# procedure\n", encoding="utf-8")
        (self.root / "rules" / "lessons.md").write_text("# lessons rule\n", encoding="utf-8")
        self.write_tokens(self.tokens_config())
        self.git("init", "-q")
        self.git("config", "user.email", "kiwi@example.test")
        self.git("config", "user.name", "Kiwi Test")
        self.git("add", ".")
        self.git("commit", "-qm", "init")

    def write_tokens(self, config):
        data = {"version": 1, "self_improve": config}
        (self.root / "tokens" / "tokens.json").write_text(
            json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    def tokens(self):
        return json.loads((self.root / "tokens" / "tokens.json").read_text(encoding="utf-8"))

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.root), *args], capture_output=True, text=True,
                              encoding="utf-8", check=True).stdout.strip()

    def ledger(self):
        return str(self.root / "brief-ledger.jsonl")


class RuleProposalTest(TempRepo):
    def tokens_config(self):
        return {"rule_proposals": {"min_group": 3, "similarity": 0.3,
                                   "area_rules": {"dev": "rules/code-review.md"}}}

    def lessons(self):
        return sorted(path for path in (self.root / "lessons").glob("*.md") if path.name != "README.md")

    def record(self, bus, index, title, correct):
        return kiwi_lessons.record_lesson(self.root, f"Mistake {index}", "2026-10-20", correct, "bot-a",
                                          area="dev", title=title, events=bus)

    def test_record_lesson_groups_similar_lessons_and_appends_updates(self):
        bus = Bus()
        rules_before = {path.name: path.read_text(encoding="utf-8") for path in (self.root / "rules").glob("*.md")}
        lesson_title = [("Forgot to run tests before commit", "Write a failing test first."),
                        ("Forgot tests before pushing", "Use the pre-commit hook."),
                        ("Forgot tests before merging", "Add a CI gate.")]
        for index, (title, correct) in enumerate(lesson_title):
            result = self.record(bus, index, title, correct)
            self.assertEqual(result["action"], "created")
        lessons = self.lessons()
        self.assertEqual(len(lessons), 3)
        proposals = sorted((self.root / "inbox" / "rule-proposals").glob("*.md"))
        self.assertEqual(len(proposals), 1, proposals)
        text = proposals[0].read_text(encoding="utf-8")
        self.assertIn("rules/code-review.md", text)
        for lesson in lessons:
            self.assertIn(lesson.relative_to(self.root).as_posix(), text)
        self.assertIn("rule_status: read-only", text)
        self.assertEqual([event for event, _ in bus.events], ["rule.proposed"])
        self.assertEqual(rules_before,
                         {path.name: path.read_text(encoding="utf-8") for path in (self.root / "rules").glob("*.md")})
        listed = si.RuleProposals(self.root).list()
        self.assertEqual(len(listed), 1)
        self.assertEqual(listed[0]["rule"], "rules/code-review.md")
        self.assertEqual(listed[0]["groups"][0]["count"], 3)

        fourth = self.record(bus, 3, "Forgot tests before release", "Run the smoke suite.")
        self.assertEqual(fourth["action"], "created")
        self.assertEqual(fourth["rule_proposals"]["written"][0]["version"], 2)
        updated = proposals[0].read_text(encoding="utf-8")
        self.assertTrue(updated.startswith(text), "updates must be append-only")
        self.assertIn("v2", updated)
        self.assertIn(fourth["path"], updated)
        self.assertEqual(len(self.lessons()), 4)
        self.assertEqual([event for event, _ in bus.events], ["rule.proposed", "rule.proposed"])

        # Same group, same lessons: a re-run is idempotent (no duplicate version).
        before = updated.count("<!-- kiwi-proposal")
        si.RuleProposals(self.root).generate()
        self.assertEqual(before, proposals[0].read_text(encoding="utf-8").count("<!-- kiwi-proposal"))

    def test_rule_proposed_is_recorded_on_the_real_event_bus(self):
        bus = EventBus(state_dir=self.root / "state")
        self.addCleanup(bus.close)
        for index, (title, correct) in enumerate(
                [("Forgot to run tests before commit", "Write a failing test first."),
                 ("Forgot tests before pushing", "Use the pre-commit hook."),
                 ("Forgot tests before merging", "Add a CI gate.")]):
            self.record(bus, index, title, correct)
        log = self.root / "state" / "kiwi-live-events.jsonl"
        events = [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]
        proposed = [event for event in events if event["event"] == "rule.proposed"]
        self.assertEqual(len(proposed), 1)
        self.assertEqual(proposed[0]["data"]["rule"], "rules/code-review.md")
        self.assertEqual(len(proposed[0]["data"]["lessons"]), 3)

    def test_below_threshold_and_retired_lessons_do_not_propose(self):
        bus = Bus()
        self.record(bus, 0, "Forgot to run tests before commit", "Write a failing test first.")
        candidate = self.root / "lessons" / "2026-10-01-retired.md"
        candidate.write_text("---\ndate: 2026-10-01\narea: dev\nstatus: retired\n---\n\n"
                             "# Forgot tests before pushing\n\n## 다음엔\n\n- Use the pre-commit hook.\n",
                             encoding="utf-8")
        self.record(bus, 1, "Forgot tests before merging", "Add a CI gate.")
        self.assertFalse((self.root / "inbox" / "rule-proposals").exists())
        self.assertEqual(bus.events, [])


class BriefFeedbackTest(TempRepo):
    def tokens_config(self):
        return {"brief_feedback": {"max_shift": 2, "missing_min_count": 2}}

    def test_feedback_reorders_selected_context_and_surfaces_missing(self):
        bus = Bus()
        sp = kiwi_sprouts.Sprouts(self.root, events=bus)
        env = mock.patch.dict(os.environ, {"KIWI_BRIEF_LEDGER": self.ledger(), "KIWI_CONTEXT_ANSWER": "off"})
        env.start()
        self.addCleanup(env.stop)
        for _ in range(2):
            result = sp.brief_feedback(principal="bot", bundle_id="b-1", useful=["c"], noise=["a"],
                                       missing="deploy steps", note="missing the deploy command")
            self.assertEqual(result["action"], "appended")
        bundle = {"bundle_id": "b-1", "mandatory_rules": [{"id": "must"}],
                  "selected_context": [{"id": "a"}, {"id": "b"}, {"id": "c"}]}
        with mock.patch.object(sp, "_context_bundle", return_value=dict(bundle)):
            out = sp.context_for("bot", "run the tests")
        self.assertEqual([item["id"] for item in out["selected_context"]], ["c", "b", "a"])
        self.assertEqual(out["mandatory_rules"], [{"id": "must"}])
        self.assertEqual(out["brief_feedback"]["max_shift"], 2)
        self.assertEqual(out["missing_repeated"], [{"text": "deploy steps", "count": 2}])
        quality = sp.brief_quality()
        self.assertEqual(quality["entries"], 2)
        self.assertEqual(quality["items"]["c"]["useful_rate"], 1.0)
        self.assertEqual(quality["items"]["a"]["useful_rate"], 0.0)
        feedback_file = next((self.root / "logs" / "brief-feedback").glob("*.jsonl"))
        rows = [json.loads(line) for line in feedback_file.read_text(encoding="utf-8").splitlines()]
        self.assertEqual(rows[0]["principal"], "bot")
        self.assertEqual(rows[0]["bundle_id"], "b-1")

    def test_reorder_is_bounded_by_max_shift(self):
        feedback = si.BriefFeedback(self.root)
        feedback.record("bot", useful=["d"], noise=["a"])
        selected = [{"id": "a"}, {"id": "b"}, {"id": "c"}, {"id": "d"}]
        with mock.patch.object(feedback, "config", return_value={**si.BRIEF_DEFAULTS, "max_shift": 1,
                                                                 "max_score": 5.0, "missing_min_count": 3,
                                                                 "useful_weight": 1.0, "noise_weight": 1.0,
                                                                 "days": 30}):
            reordered, meta = feedback.reorder(selected, "bot")
        original = {item["id"]: index for index, item in enumerate(selected)}
        for index, item in enumerate(reordered):
            self.assertLessEqual(abs(index - original[item["id"]]), 1, reordered)
        self.assertEqual(meta["max_shift"], 1)
        self.assertEqual(sorted(item["id"] for item in reordered), ["a", "b", "c", "d"])

    def test_secret_and_empty_feedback_are_rejected(self):
        feedback = si.BriefFeedback(self.root)
        with self.assertRaises(ValueError):
            feedback.record("bot")
        with self.assertRaises(ValueError):
            feedback.record("bot", note="api_key = sk-abcdefghijklmnopqrstuv")
        self.assertFalse((self.root / "logs" / "brief-feedback").exists())


@unittest.skipUnless((kiwi_sprouts.KIWI / "tools" / "decisions" / "ask.py").is_file(), "tools/decisions not in this checkout")
class CalibrationTest(TempRepo):
    def tokens_config(self):
        return {"calibration": {"threshold": 0.75, "threshold_min": 0.5, "threshold_max": 0.95,
                                "step": 0.05, "min_samples": 4, "min_bucket": 1,
                                "target_accuracy": 0.9, "days": 30,
                                "buckets": [0.0, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0]}}

    def setUp(self):
        super().setUp()
        self.home = Path(tempfile.mkdtemp(prefix="kiwi-calibration-state-"))
        self.addCleanup(shutil.rmtree, self.home, ignore_errors=True)
        env = mock.patch.dict(os.environ, {"KIWI_HOME": str(self.home),
                                           "KIWI_VERDICTS": str(self.home / "verdicts.jsonl"),
                                           "KIWI_BRIEF_LEDGER": self.ledger()})
        env.start()
        self.addCleanup(env.stop)

    def ask_module(self):
        import ask
        return ask

    def resolve_choices(self, hits, misses):
        ask = self.ask_module()
        resolved = []
        for index in range(hits + misses):
            actual = "a" if index < hits else "b"
            record = ask.record_decide("choice", f"question {index}", "input", ["a", "b"],
                                       {"choice": "a", "probabilities": {"a": 0.85, "b": 0.15}}, "bot")
            self.assertEqual(record["confidence"], 0.85)
            resolved.append(ask.resolve(record["ask_id"], actual, kiwi=self.root))
        return resolved

    def test_decide_prediction_resolves_with_ask_resolve_and_moves_threshold(self):
        resolved = self.resolve_choices(hits=3, misses=1)
        self.assertEqual([item["hit"] for item in resolved], [True, True, True, False])
        self.assertTrue(all(item["verdict"] is None for item in resolved))
        self.assertFalse((self.home / "verdicts.jsonl").exists())

        bus = Bus()
        sp = kiwi_sprouts.Sprouts(self.root, events=bus)
        report = sp.decide_calibration(days=30)
        self.assertEqual(report["resolved"], 4)
        self.assertEqual(report["accuracy"], 0.75)
        self.assertEqual(report["buckets"][4]["total"], 4)  # 0.8..0.9
        self.assertEqual(report["threshold"]["action"], "raise")
        self.assertEqual(report["threshold"]["recommended"], 0.8)
        self.assertEqual(self.tokens()["self_improve"]["calibration"]["threshold"], 0.8)
        self.assertIn("token.changed", [event for event, _ in bus.events])

        # The ask path now uses the moved threshold from tokens, not a literal.
        ask = self.ask_module()
        with mock.patch.object(ask, "ask", return_value={"status": "answered"}) as mocked:
            sp.ask("a question")
            self.assertEqual(mocked.call_args.kwargs["threshold"], 0.8)

    def test_insufficient_evidence_holds_threshold(self):
        ask = self.ask_module()
        sp = kiwi_sprouts.Sprouts(self.root, events=Bus())
        row = {"confidence": 0.9, "hit": True, "basis": "decide:choice", "predicted": "a", "actual": "a"}
        with mock.patch.object(ask, "calibration", return_value={"days": 30, "resolved": 1, "rows": [row]}):
            report = sp.decide_calibration(days=30)
        self.assertEqual(report["threshold"]["action"], "hold")
        self.assertEqual(report["threshold"]["reason"], "insufficient_evidence")
        self.assertEqual(self.tokens()["self_improve"]["calibration"]["threshold"], 0.75)
        self.assertIsNone(report["change"])

    def test_sprouts_decide_records_an_outcome_that_ask_resolve_can_close(self):
        import decide
        sp = kiwi_sprouts.Sprouts(self.root, events=Bus())
        with mock.patch.object(decide, "decide", return_value={"choice": "b", "probabilities": {"b": 0.7}}):
            result = sp.decide("choice", "pick one", "input", ["a", "b"], principal="bot")
        self.assertEqual(result["calibration"]["predicted"], "b")
        self.assertEqual(result["ask_id"], result["calibration"]["ask_id"])
        self.assertIn("prediction", result)
        closed = sp.ask_resolve(result["ask_id"], "b")
        self.assertTrue(closed["hit"])
        self.assertIsNone(closed["verdict"])


class ExposureTest(TempRepo):
    def test_new_tools_are_registered_for_stdio_and_remote_http(self):
        for name in ("rule_proposals", "brief_feedback", "brief_quality", "decide_calibration"):
            self.assertIn(name, kiwi_sprouts.TOOLS)
            self.assertIn(name, kiwi_live.HTTP_TOOLS)
        for name in ("brief_feedback", "decide_calibration"):
            self.assertIn(name, kiwi_brief.GUARDED_TOOLS)

    def test_kiwi_status_includes_self_improve_summary(self):
        sp = kiwi_sprouts.Sprouts(self.root, events=Bus())
        with mock.patch.object(sp, "_kiwi", return_value={"ok": True}):
            status = sp.kiwi_status()
        self.assertEqual(status["ok"], True)
        self.assertIn("self_improve", status)
        for key in ("rule_proposals", "brief_feedback_30d", "threshold"):
            self.assertIn(key, status["self_improve"])


if __name__ == "__main__":
    unittest.main()
