import contextlib
import io
import json
import shutil
import sys
import tempfile
import unittest
import uuid
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
import kiwi_dsh_host as dsh


def token_source():
    return {
        "version": 1,
        "dsh_host": {
            "coding_route": {
                "usage_dir": "usage",
                "priority": [
                    {
                        "id": "claude-opus-5.5",
                        "model": "claude-opus-5.5",
                        "effort": "xhigh",
                        "tool": "Claude Code",
                        "account": "owner@example.com",
                        "usage_report": "claude-opus-5.5.json",
                        "exhausted_field": "exhausted",
                    },
                    {
                        "id": "deepseek",
                        "model": "deepseek",
                        "tool": "dsh (@deepseek-ai/dsh)",
                        "usage_report": "deepseek.json",
                        "exhausted_field": "exhausted",
                    },
                ],
            },
            "code_review_account": {
                "email": "owner@example.com",
                "note": "automatic code review by 오너 personally",
            },
            "candidates": [
                {
                    "id": "windows-studio-pc",
                    "tailscale": "studio-pc",
                    "machine_id": "00000000-0000-4000-8000-6f5c571ba078",
                    "ready": True,
                    "note": "dsh 0.2.1-rc.2 installed and tested",
                },
                {"id": "box", "tailscale": "box-host", "local": True},
                {
                    "id": "mac-mini",
                    "tailscale": "mac-host",
                    "machine_id": "00000000-0000-4000-8000-e361d915d2ec",
                    "mac_only": True,
                },
            ],
            "mac_only_tasks": ["simulator", "Xcode final build", "App Store submission", "iPhone mirroring"],
            "thresholds": {
                "cpu_percent": 85,
                "memory_percent": 85,
                "max_running_dsh": 2,
                "report_stale_seconds": 900,
            },
        },
    }


class HostSelection(unittest.TestCase):
    def setUp(self):
        # TemporaryDirectory uses mode 0700; on Windows that can leave a
        # directory the current process cannot create children in.
        self.temporary = Path(tempfile.gettempdir()) / ("kiwi-dsh-host-" + uuid.uuid4().hex)
        self.temporary.mkdir(parents=True)
        self.root = self.temporary / "kiwi"
        (self.root / "tokens").mkdir(parents=True)
        self.reports = self.temporary / "host-load"
        self.usage = self.temporary / "usage"
        self.now = 1_800_000_000.0
        self.write_tokens(token_source())
        self.local_load = {
            "cpu_percent": 5.0,
            "memory_percent": 10.0,
            "running_dsh": 0,
            "measured_at": dsh._iso(self.now),
            "epoch": self.now,
        }

    def tearDown(self):
        shutil.rmtree(self.temporary, ignore_errors=True)

    def write_tokens(self, data):
        (self.root / "tokens" / "tokens.json").write_text(
            json.dumps(data, ensure_ascii=False), encoding="utf-8"
        )

    def set_windows_ready(self, ready=True):
        data = token_source()
        for candidate in data["dsh_host"]["candidates"]:
            if candidate["id"] == "windows-studio-pc":
                candidate["ready"] = ready
        self.write_tokens(data)

    def report(self, host, **values):
        record = {
            "host": host,
            "cpu_percent": 10.0,
            "memory_percent": 20.0,
            "running_dsh": 0,
            "measured_at": dsh._iso(self.now),
            "epoch": self.now,
        }
        record.update(values)
        dsh.write_report(host, record, self.reports)

    def write_usage(self, name, record):
        self.usage.mkdir(parents=True, exist_ok=True)
        (self.usage / name).write_text(json.dumps(record), encoding="utf-8")

    def select(self, mac_only=False, local_load=None, hostname=None):
        return dsh.select_host(
            mac_only,
            root=self.root,
            reports=self.reports,
            usage=self.usage,
            local_loader=lambda: dict(local_load or self.local_load),
            now=self.now,
            hostname=hostname,
        )

    def test_fresh_ready_windows_is_first(self):
        self.report("windows-studio-pc", cpu_percent=40.0)
        result = self.select()
        self.assertEqual(result["host"], "windows-studio-pc")
        self.assertEqual(result["tailscale"], "studio-pc")
        self.assertIn("ready, fresh and under thresholds", result["reason"])
        self.assertEqual(result["model"], "claude-opus-5.5")
        self.assertEqual(result["model_effort"], "xhigh")
        self.assertEqual(result["model_tool"], "Claude Code")

    def test_windows_not_ready_falls_back_to_box(self):
        self.set_windows_ready(False)
        result = self.select()
        self.assertEqual(result["host"], "box")
        self.assertEqual(result["tailscale"], "box-host")
        self.assertIn("windows-studio-pc skipped: not ready", result["reason"])
        self.assertIn("box is ready", result["reason"])
        self.assertEqual(result["model"], "claude-opus-5.5")

    def test_windows_local_host_is_measured_without_a_report(self):
        data = token_source()
        data["dsh_host"]["candidates"][0]["local_host"] = "studio-pc"
        self.write_tokens(data)
        result = self.select(hostname="studio-pc")
        self.assertEqual(result["host"], "windows-studio-pc")
        self.assertEqual(result["load"]["source"], "local")

    def test_stale_report_falls_back_to_box(self):
        self.report("windows-studio-pc", measured_at=dsh._iso(self.now - 1000), epoch=self.now - 1000)
        result = self.select()
        self.assertEqual(result["host"], "box")
        self.assertIn("load report stale", result["reason"])

    def test_over_threshold_falls_back_to_box(self):
        self.report("windows-studio-pc", cpu_percent=99.0)
        result = self.select()
        self.assertEqual(result["host"], "box")
        self.assertIn("cpu 99% over 85% threshold", result["reason"])

    def test_over_memory_or_dsh_threshold_falls_back_to_box(self):
        for values, needle in (
            ({"memory_percent": 99.0}, "memory 99% over 85% threshold"),
            ({"running_dsh": 3}, "3 dsh running over max 2"),
        ):
            with self.subTest(values=values):
                self.set_windows_ready()
                self.report("windows-studio-pc", **values)
                result = self.select()
                self.assertEqual(result["host"], "box")
                self.assertIn(needle, result["reason"])

    def test_never_mac_mini_for_non_mac_work(self):
        self.local_load["cpu_percent"] = 99.0
        result = self.select(local_load=self.local_load)
        self.assertIsNone(result["host"])
        self.assertNotEqual(result["host"], "mac-mini")
        self.assertIn("mac-mini is never used for non-mac work", result["reason"])

    def test_mac_only_uses_mac_mini_without_a_report(self):
        result = self.select(mac_only=True)
        self.assertEqual(result["host"], "mac-mini")
        self.assertEqual(result["tailscale"], "mac-host")
        self.assertIn("Mac-only task", result["reason"])
        self.assertEqual(result["model"], "claude-opus-5.5")

    def test_config_comes_from_tokens(self):
        data = token_source()
        data["dsh_host"]["candidates"] = [
            {"id": "box", "tailscale": "box-host", "local": True},
            {
                "id": "mac-mini",
                "tailscale": "mac-host",
                "mac_only": True,
            },
        ]
        self.write_tokens(data)
        result = self.select()
        self.assertEqual(result["host"], "box")
        self.assertEqual([c["id"] for c in dsh.load_config(self.root)["candidates"]], ["box", "mac-mini"])

    def test_opus_wins_before_deepseek_when_usage_is_unknown(self):
        result = self.select()
        self.assertEqual(result["model"], "claude-opus-5.5")
        self.assertEqual(result["model_effort"], "xhigh")
        self.assertEqual([item["id"] for item in result["route"]], ["claude-opus-5.5", "deepseek"])
        self.assertIn("no usage report", result["model_reason"])

    def test_exhausted_opus_switches_to_deepseek(self):
        self.write_usage("claude-opus-5.5.json", {"exhausted": True})
        result = self.select()
        self.assertEqual(result["model"], "deepseek")
        self.assertEqual(result["model_tool"], "dsh (@deepseek-ai/dsh)")
        self.assertIn("claude-opus-5.5 skipped: usage report says exhausted", result["model_reason"])

    def test_opus_usage_flag_in_tokens_switches_to_deepseek(self):
        data = token_source()
        data["dsh_host"]["coding_route"]["priority"][0]["exhausted"] = True
        self.write_tokens(data)
        result = self.select()
        self.assertEqual(result["model"], "deepseek")
        self.assertIn("token flag says exhausted", result["model_reason"])

    def test_opus_remaining_percent_zero_switches_to_deepseek(self):
        self.write_usage("claude-opus-5.5.json", {"remaining_percent": 0})
        result = self.select()
        self.assertEqual(result["model"], "deepseek")

    def test_opus_future_exhausted_until_switches_and_past_reset_returns(self):
        self.write_usage(
            "claude-opus-5.5.json",
            {"exhausted": True, "exhausted_until": dsh._iso(self.now + 600)},
        )
        self.assertEqual(self.select()["model"], "deepseek")
        self.write_usage(
            "claude-opus-5.5.json",
            {"exhausted_until": dsh._iso(self.now + 600)},
        )
        self.assertEqual(self.select()["model"], "deepseek")
        self.write_usage(
            "claude-opus-5.5.json",
            {"exhausted": True, "exhausted_until": dsh._iso(self.now - 60)},
        )
        self.assertEqual(self.select()["model"], "claude-opus-5.5")

    def test_all_models_exhausted_returns_no_model(self):
        self.write_usage("claude-opus-5.5.json", {"exhausted": True})
        self.write_usage("deepseek.json", {"exhausted": True})
        result = self.select()
        self.assertIsNone(result["model"])
        self.assertIn("all coding models exhausted", result["reason"])
        self.assertEqual(result["host"], "box")

    def test_report_writes_local_file_then_posts_with_bearer(self):
        seen = {}

        class Reply:
            status = 200

            def read(self):
                return b'{"host":"box"}'

            def __enter__(self):
                return self

            def __exit__(self, *exc):
                return False

        def opener(request, timeout=0):
            seen["url"] = request.full_url
            seen["auth"] = request.get_header("Authorization")
            seen["body"] = json.loads(request.data)
            seen["timeout"] = timeout
            return Reply()

        result = dsh.report_host(
            "box", root=self.root, reports=self.reports,
            url="https://kiwi-host.example.net/mcp", token="t" * 40,
            opener=opener, now=self.now,
        )
        stored = json.loads((self.reports / "box.json").read_text(encoding="utf-8"))
        self.assertEqual(stored["host"], "box")
        self.assertEqual(stored["epoch"], self.now)
        self.assertEqual(seen["url"], "https://kiwi-host.example.net/host-load")
        self.assertEqual(seen["auth"], "Bearer " + "t" * 40)
        self.assertEqual(seen["body"]["host"], "box")
        self.assertEqual(result["status"], 200)

    def test_unknown_report_host_is_rejected(self):
        with self.assertRaises(ValueError):
            dsh.report_host("../../etc/passwd", root=self.root, reports=self.reports,
                            url="http://127.0.0.1/mcp", token="t" * 40,
                            opener=lambda *a, **k: self.fail("must not post"))

    def test_cli_json_output(self):
        self.report("windows-studio-pc")
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = dsh.main([
                "select", "--json", "--root", str(self.root),
                "--reports-dir", str(self.reports), "--usage-dir", str(self.usage),
            ])
        self.assertEqual(code, 0)
        payload = json.loads(out.getvalue())
        self.assertEqual(payload["host"], "windows-studio-pc")
        self.assertEqual(payload["model"], "claude-opus-5.5")

    def test_cli_returns_1_when_no_model_is_available(self):
        self.write_usage("claude-opus-5.5.json", {"exhausted": True})
        self.write_usage("deepseek.json", {"exhausted": True})
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = dsh.main([
                "select", "--json", "--root", str(self.root),
                "--reports-dir", str(self.reports), "--usage-dir", str(self.usage),
            ])
        self.assertEqual(code, 1)
        self.assertIsNone(json.loads(out.getvalue())["model"])


if __name__ == "__main__":
    unittest.main()
