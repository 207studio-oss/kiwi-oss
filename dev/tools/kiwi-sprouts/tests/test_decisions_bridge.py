import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


class DecisionsBridge(unittest.TestCase):
    def test_ask_stats_allows_only_days_and_wraps_system_exit(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            folder = root / "tools/decisions"
            folder.mkdir(parents=True)
            (folder / "ask.py").write_text(
                "def stats(days=30, write=None, **kwargs):\n"
                "    if days == 17: raise SystemExit('not configured')\n"
                "    if write is not None: raise AssertionError('write reached stats')\n"
                "    return dict(days=days, kwargs=kwargs)\n", encoding="utf-8")
            arguments = [{}, {"days": 7}, {"write": "report.md"}, {"days": 7, "other": True}, {"days": 17}, {"days": 1}]
            requests = [{"root": str(root), "method": "ask_stats", "args": args} for args in arguments]
            result = subprocess.run([sys.executable, str(Path(__file__).resolve().parents[1] / "src/decisions.py")],
                                    input="".join(json.dumps(request) + "\n" for request in requests),
                                    capture_output=True, text=True, encoding="utf-8", timeout=15)
            self.assertFalse((root / "report.md").exists())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        replies = [json.loads(line) for line in result.stdout.splitlines()]
        self.assertEqual(replies[:2], [{"result": {"days": 30, "kwargs": {}}}, {"result": {"days": 7, "kwargs": {}}}])
        for index, key in ((2, "write"), (3, "other")):
            self.assertEqual(replies[index], {"error": f"Sprouts.ask_stats() got an unexpected keyword argument '{key}'"})
        self.assertIn("Decisions not configured on this machine (not configured)", replies[4]["error"])
        self.assertEqual(replies[5], {"result": {"days": 1, "kwargs": {}}})

    def test_extra_keys_are_rejected_and_system_exit_does_not_kill_worker(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            folder = root / "tools/decisions"
            folder.mkdir(parents=True)
            (folder / "ask.py").write_text(
                "def ask(question, options=None, task='', principal='', **kwargs):\n"
                "    if question == 'exit': raise SystemExit('not configured')\n"
                "    return dict(question=question, kwargs=kwargs)\n"
                "def resolve(ask_id, actual, **kwargs):\n"
                "    if ask_id == 'exit': raise SystemExit('not configured')\n"
                "    return dict(ask_id=ask_id, actual=actual, kwargs=kwargs)\n", encoding="utf-8")
            requests = []
            for method, args in [("ask", {"question": "q"}), ("ask_resolve", {"ask_id": "id", "actual": "a"})]:
                for key, value in [("threshold", 0), ("kiwi", "elsewhere"), ("opener", "callback")]:
                    requests.append({"root": str(root), "method": method, "args": {**args, key: value}})
            requests.extend([
                {"root": str(root), "method": "ask", "args": {"question": "exit"}},
                {"root": str(root), "method": "ask_resolve", "args": {"ask_id": "exit", "actual": "a"}},
                {"root": str(root), "method": "ask", "args": {"question": "survived"}},
            ])
            result = subprocess.run([sys.executable, str(Path(__file__).resolve().parents[1] / "src/decisions.py")],
                                    input="".join(json.dumps(request) + "\n" for request in requests),
                                    env={**os.environ, "PYTHONIOENCODING": "utf-8"},
                                    capture_output=True, text=True, encoding="utf-8", timeout=15)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        replies = [json.loads(line) for line in result.stdout.splitlines()]
        self.assertEqual(len(replies), len(requests))
        for request, reply in zip(requests[:6], replies[:6]):
            key = next(key for key in request["args"] if key in ("threshold", "kiwi", "opener"))
            self.assertEqual(reply["error"], f"Sprouts.{request['method']}() got an unexpected keyword argument '{key}'")
        for reply in replies[6:8]:
            self.assertIn("Decisions not configured on this machine (not configured)", reply["error"])
            self.assertNotIn("fatal", reply)
        self.assertEqual(replies[-1], {"result": {"question": "survived", "kwargs": {}}})


if __name__ == "__main__":
    unittest.main()
