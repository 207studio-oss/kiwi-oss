import json, os, shutil, subprocess, sys, tempfile, unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path[:0] = [str(HERE), str(HERE.parent)]
import sprouts_support as ks

FX = HERE / "fixtures"


def copy(lang):
    d = Path(tempfile.mkdtemp()) / lang
    shutil.copytree(FX / lang, d)
    return d


def missing(exe):
    """Absent, or a rustup proxy whose component is not installed."""
    path = shutil.which(exe)
    if not path or exe != "rust-analyzer": return not path
    try: return subprocess.run([path, "--version"], capture_output=True, timeout=60).returncode != 0
    except (OSError, subprocess.TimeoutExpired): return True


class Lang(unittest.TestCase):
    def run_lang(self, lang, exe, file, sym, use_line, use_col, n_refs):
        if missing(exe): self.skipTest(exe + " missing")
        sp = ks.Sprouts(copy(lang))
        try:
            names = [s["name"].rsplit("/", 1)[-1] for s in sp.code_overview(file)]
            self.assertIn(sym, names)
            self.assertTrue(any(h["name"].endswith(sym) for h in sp.code_find(sym)))
            d = sp.code_definition(file, use_line, use_col)
            self.assertTrue(d and d[0]["path"].endswith(Path(file).name), d)
            refs = sp.code_references(file, d[0]["line"], d[0]["col"])
            self.assertGreaterEqual(len(refs), n_refs, refs)
        finally: sp.close()

    def test_python(self): self.run_lang("py", "pyright-langserver", "app.py", "greet", 7, 22, 1)
    def test_typescript(self): self.run_lang("ts", "typescript-language-server", "app.ts", "add", 5, 22, 2)
    def test_rust(self): self.run_lang("rs", "rust-analyzer", "src/main.rs", "double", 6, 20, 1)
    def test_swift(self): self.run_lang("swift", "sourcekit-lsp", "Sources/fx/main.swift", "triple", 5, 7, 1)

    def test_replace_body(self):
        if not shutil.which("pyright-langserver"): self.skipTest("pyright")
        root = copy("py"); sp = ks.Sprouts(root)
        try: sp.code_replace_body("app.py", "main", "def main():\n    return 42")
        finally: sp.close()
        t = (root / "app.py").read_text()
        self.assertIn("return 42", t); self.assertIn("class Greeter", t)

    def test_path_escape(self):
        with self.assertRaises(ValueError): ks.Sprouts(FX).abs("../../x")


class Mcp(unittest.TestCase):
    def test_file_uri_roundtrip(self):
        import kiwi_sprouts
        path = (HERE / "space 한글.py").resolve()
        self.assertEqual(Path(kiwi_sprouts.unuri(kiwi_sprouts.uri(path))), path)
        self.assertEqual(kiwi_sprouts.uri(path), path.as_uri())

    def test_stdio_no_listener(self):
        msgs = [{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}},
                {"jsonrpc": "2.0", "method": "notifications/initialized"},
                {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
                {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "ops_graph", "arguments": {"args": ["build"]}}},
                {"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "docs_search", "arguments": {"query": "Kiwi", "k": 1}}}]
        env = dict(os.environ, KIWI_DOCS_SEARCH=str(HERE / "fixtures" / ("fake-docs-search.cmd" if os.name == "nt" else "fake-docs-search")), PYTHONIOENCODING="utf-8")
        r = subprocess.run([*ks.command(), "mcp"], input="\n".join(map(json.dumps, msgs)),
                           capture_output=True, text=True, encoding="utf-8", timeout=60, env=env)
        out = [json.loads(l) for l in r.stdout.splitlines()]
        self.assertEqual(out[0]["result"]["serverInfo"]["name"], "kiwi-sprouts")
        self.assertEqual({t["name"] for t in out[1]["result"]["tools"]}, set(ks.TOOLS))
        self.assertTrue(out[2]["result"]["isError"])  # build (write) is refused
        self.assertIn("fake-hit", out[3]["result"]["content"][0]["text"])
        src = (HERE.parent / "kiwi_sprouts.py").read_text(encoding="utf-8")
        for bad in ("import socket", "http.server", "webbrowser", "bind("): self.assertNotIn(bad, src)


if __name__ == "__main__": unittest.main()
