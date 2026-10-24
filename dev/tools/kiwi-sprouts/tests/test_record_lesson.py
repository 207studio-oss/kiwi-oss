import json
import shutil
import subprocess
import sys
import tempfile
import unittest
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import kiwi_lessons


class RecordLessonTest(unittest.TestCase):
    def setUp(self):
        # tempfile.mkdtemp creates mode 0700, which this Windows sandbox denies to child processes.
        self.root = Path(tempfile.gettempdir()) / ("kiwi-lessons-" + uuid.uuid4().hex)
        self.root.mkdir()
        self.addCleanup(shutil.rmtree, self.root, ignore_errors=True)
        self.git("init", "-q")
        self.git("config", "user.name", "Test Bot")
        self.git("config", "user.email", "bot@example.test")
        (self.root / "lessons").mkdir()
        (self.root / "lessons" / "README.md").write_text("# lessons\n", encoding="utf-8")
        self.git("add", "lessons/README.md")
        self.git("commit", "-q", "-m", "init")

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.root), *args], capture_output=True, text=True,
                              encoding="utf-8", check=True).stdout.strip()

    def lesson_files(self):
        return sorted(p for p in (self.root / "lessons").glob("*.md") if p.name != "README.md")

    def test_creates_lesson_in_exact_format_and_commits(self):
        result = kiwi_lessons.record_lesson(
            self.root, "I forgot to run tests.", "2026-10-20", "Run tests before committing.", "bot-a",
            where="logs/x.md:3", area="dev", title="Forgot tests")
        self.assertEqual(result["action"], "created")
        self.assertEqual(result["path"], f"lessons/{kiwi_lessons._today()}-forgot-tests.md")
        self.assertEqual(result["commit"], self.git("rev-parse", "HEAD"))
        path = self.root / result["path"]
        expected = (
            "---\n"
            f"date: {kiwi_lessons._today()}\n"
            "recorded_by: bot-a\n"
            "area: dev\n"
            "status: open\n"
            f"occurrences: {json.dumps(['2026-10-20 logs/x.md:3 (bot-a 보고)'], ensure_ascii=False)}\n"
            "---\n\n"
            "# Forgot tests\n\n"
            "## 무엇이\n\n"
            "I forgot to run tests.\n\n"
            "근거: logs/x.md:3\n\n"
            "## 이제\n\n"
            "- Run tests before committing.\n"
        )
        self.assertEqual(path.read_text(encoding="utf-8"), expected)
        self.assertEqual(self.git("log", "-1", "--format=%s"), "lessons: Forgot tests (record_lesson, bot-a)")

    def test_dedupes_into_occurrences_and_reopens_fixed_lesson(self):
        first = kiwi_lessons.record_lesson(
            self.root, "Writes raced.", "2026-10-20", "Use the lock around writes.", "bot-a", title="Same mistake")
        path = self.root / first["path"]
        path.write_text(path.read_text(encoding="utf-8").replace("status: open", "status: fixed"), encoding="utf-8")
        second = kiwi_lessons.record_lesson(
            self.root, "It happened again.", "2026-10-21", "Run the tests.", "bot-b", title=" same   MISTAKE ")
        self.assertEqual(second["action"], "appended")
        self.assertEqual(second["path"], first["path"])
        text = path.read_text(encoding="utf-8")
        self.assertIn("status: open", text)
        self.assertNotIn("status: fixed", text)
        self.assertIn("date: " + kiwi_lessons._today(), text)
        self.assertIn("recorded_by: bot-a", text)
        self.assertIn("area: general", text)
        lines = text.splitlines()
        occurrences_line = next(line for line in lines if line.startswith("occurrences: "))
        self.assertEqual(json.loads(occurrences_line.partition(": ")[2]), [
            "2026-10-20  (bot-a 보고)",
            "2026-10-21  (bot-b 보고)",
        ])
        third = kiwi_lessons.record_lesson(
            self.root, "Same correction, different title.", "2026-10-22", "Use the lock around writes.",
            "bot-c", title="Different title")
        self.assertEqual(third["action"], "appended")
        self.assertEqual(third["path"], first["path"])
        self.assertEqual(len(self.lesson_files()), 1)

    def test_secret_is_rejected_before_writing(self):
        base = dict(what="Something happened.", when="2026-10-20", correct="Do better.", reporter="bot-a")
        secret = "api_key = sk-abcdefghijklmnopqrstuv"
        for field in ("what", "when", "correct", "reporter", "where", "area", "title"):
            with self.subTest(field=field):
                args = dict(base)
                args[field] = secret
                with self.assertRaises(ValueError):
                    kiwi_lessons.record_lesson(self.root, **args)
                self.assertEqual(self.lesson_files(), [])

    def test_student_data_is_rejected(self):
        base = dict(what="Something happened.", when="2026-10-20", correct="Do better.", reporter="bot-a")
        for value in ("900101-1234567", "010-1234-5678", "student@gmail.com", "학생 이름", "student name"):
            with self.subTest(value=value):
                args = dict(base, what=value)
                with self.assertRaises(ValueError):
                    kiwi_lessons.record_lesson(self.root, **args)
                self.assertEqual(self.lesson_files(), [])

    def test_reporter_with_path_separator_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "reporter must be an agent id or account name"):
            kiwi_lessons.record_lesson(
                self.root, "Something happened.", "2026-10-20", "Do better.", "../x")
        with self.assertRaisesRegex(ValueError, "reporter must be an agent id or account name"):
            kiwi_lessons.record_decision(self.root, "Use PostgreSQL", "../x")
        self.assertEqual(self.lesson_files(), [])
        self.assertFalse((self.root / "logs").exists())

    def test_decision_appends_and_dedupes(self):
        first = kiwi_lessons.record_decision(self.root, "Use PostgreSQL", "bot-a")
        self.assertEqual(first["action"], "appended")
        self.assertEqual(first["commit"], self.git("rev-parse", "HEAD"))
        duplicate = kiwi_lessons.record_decision(self.root, "Use PostgreSQL", "bot-a")
        self.assertEqual(duplicate, {"action": "skipped", "path": first["path"], "commit": None})
        second = kiwi_lessons.record_decision(self.root, "Use WAL", "bot-a")
        self.assertEqual(second["action"], "appended")
        self.assertEqual(second["path"], first["path"])
        text = (self.root / first["path"]).read_text(encoding="utf-8")
        day = kiwi_lessons._today()
        self.assertEqual(text, f"# decisions inbox {day} bot-a\n\n- Use PostgreSQL\n- Use WAL\n")
        self.assertEqual(self.git("log", "-1", "--format=%s"), "logs: decision (record_decision, bot-a)")


if __name__ == "__main__":
    unittest.main()
