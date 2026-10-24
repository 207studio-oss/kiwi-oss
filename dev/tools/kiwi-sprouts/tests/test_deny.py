"""Sensitive paths are readable and searchable (오너 2026-10-23 17:54). Only .git is excluded.
The Rust kiwi-sprouts port must keep the same behaviour."""
import os, sys, tempfile, unittest
from pathlib import Path
from unittest import mock

sys.path[:0] = [str(Path(__file__).resolve().parent), str(Path(__file__).resolve().parent.parent)]
import sprouts_support as ks

SENSITIVE = ["accounts/openai.md", "secrets/s.md", ".env", ".env.local", "k.key", "c.pem", "sales/accounts/polar.md"]

class SensitiveReadable(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp()).resolve()
        for rel in SENSITIVE + ["rules/ok.md", "accounts/x.py", ".git/config.md"]:
            f = self.root / rel; f.parent.mkdir(parents=True, exist_ok=True)
            f.write_text("ZEBRATOKEN = 1\n")

    def test_only_git_is_denied(self):
        for rel in SENSITIVE + ["rules/ok.md"]:
            self.assertFalse(ks.denied(rel), rel)
        self.assertTrue(ks.denied(".git/config"))

    def test_rg_fallback_returns_sensitive_files_but_not_git(self):
        if not ks.shutil.which("rg"): self.skipTest("rg missing")
        previous = os.getcwd()
        os.chdir(self.root)
        self.addCleanup(os.chdir, previous)
        with mock.patch.object(ks, "KIWI", Path(".")), mock.patch.dict(os.environ, {"KIWI_DOCS_SEARCH": ""}), \
             mock.patch.object(ks.Path, "home", return_value=self.root / "nohome"):
            out = ks.Sprouts(self.root).docs_search("ZEBRATOKEN", k=50)["out"]
        out = out.replace("\\", "/")
        for rel in SENSITIVE + ["rules/ok.md"]:
            self.assertIn(rel, out)
        self.assertNotIn(".git/config",out)

    def test_brain_search_output_not_filtered(self):
        tool = self.root / ("fake-search.cmd" if os.name == "nt" else "fake-search")
        tool.write_text("@echo off\necho accounts/openai.md: key\necho rules/ok.md: fine\n" if os.name == "nt" else "#!/bin/sh\necho 'accounts/openai.md: key'\necho 'rules/ok.md: fine'\n"); tool.chmod(0o755)
        with mock.patch.dict(os.environ, {"KIWI_DOCS_SEARCH": str(tool)}):
            out = ks.Sprouts(self.root).docs_search("x")["out"]
        self.assertIn("accounts/openai.md", out); self.assertIn("rules/ok.md", out)

    def test_code_paths_allow_accounts(self):
        sp = ks.Sprouts(self.root)
        self.addCleanup(sp.close)
        self.assertEqual(sp.abs("accounts/x.py"),self.root/"accounts/x.py")
        self.assertTrue(any("accounts" in str(f) for f in sp._files(".")))
        self.assertFalse(any(".git" in f.parts for f in sp._files(".")))


if __name__ == "__main__":
    unittest.main()
