import collections
import codecs
from contextlib import closing
import json
import locale
import os
import random
import struct
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import threading
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(HERE))
import kiwi_sprouts as oracle

BINARY = os.environ.get("KIWI_SPROUTS_BIN")
COVERAGE = collections.Counter()


def request(tool, **args):
    return {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}}


@unittest.skipUnless(BINARY, "set KIWI_SPROUTS_BIN to the Rust binary")
class Parity(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="sprouts-parity-")
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.root = self.base / "workspace"
        self.root.mkdir()
        self.bin = self.base / "bin"
        self.bin.mkdir()
        self.config = self.base / "fixture.json"
        self.rg_config = self.base / "ripgreprc"
        self.rg_config.write_text("--sort\npath\n",encoding="utf-8")
        self.fixture = Path(BINARY).parent / ("kiwi-sprouts-fixture.exe" if os.name == "nt" else "kiwi-sprouts-fixture")
        self.assertTrue(self.fixture.is_file(), "cargo build --bins first")
        self.suffix = ".exe" if os.name == "nt" else ""
        for name in ["kiwi", "kiwi-graph", "kiwi-search", "fake-docs-search", "pyright-langserver", "typescript-language-server", "rust-analyzer", "sourcekit-lsp"]:
            os.link(self.fixture, self.bin / (name + self.suffix))
        self.source = "def greet(name):\n    return name\n\nprint(greet('키위'))\n"
        (self.root / "app.py").write_text(self.source, encoding="utf-8")
        self.env = dict(os.environ, KIWI_BIN=str(self.bin / ("kiwi" + self.suffix)),
                        KIWI_GRAPH_BIN=str(self.bin / ("kiwi-graph" + self.suffix)),
                        KIWI_DOCS_SEARCH=str(self.bin / ("kiwi-search" + self.suffix)),
                        KIWI_ROOT=str(self.root), KIWI_SPROUTS_ROOT=str(self.root),
                        KIWI_DB=str(self.base / "kiwi.db"), KIWI_HOME=str(self.base / "home"),
                        KIWI_DECISIONS="off", SPROUTS_FIXTURE_DATA=str(self.config),
                        PATH=str(self.bin) + os.pathsep + os.environ.get("PATH", ""), PYTHONIOENCODING="utf-8", PYTHONUTF8="1",RIPGREP_CONFIG_PATH=str(self.rg_config))
        self.env["USERPROFILE" if os.name == "nt" else "HOME"] = str(self.base / "user")
        self.configuration = {"docs": {"out": "rules/ok.md: Kiwi 키위 🌱\naccounts/no.md: private\n"}}
        self.write_config()

    def write_config(self):
        self.config.write_text(json.dumps(self.configuration, ensure_ascii=False), encoding="utf-8")

    def pair(self, messages, env=None, reset=None, cli=None, cwd=None, newline="\n", raw=None, timeout=90):
        environment = {**self.env, **(env or {})}
        for index, message in enumerate(messages):
            if message.get("id") is not None:
                message["id"] = index + 1
            if message.get("method") == "tools/call" and isinstance(message.get("params", {}).get("name"), str):
                COVERAGE[message["params"]["name"]] += 1
        results = []
        effects = []
        encoding=environment.get("PYTHONIOENCODING","").split(":")[0] or ("utf-8" if environment.get("PYTHONUTF8")=="1" else locale.getencoding())
        for command in [[sys.executable, str(HERE / "kiwi_sprouts.py")], [BINARY]]:
            if reset:
                reset()
            result = subprocess.run([*command, *(cli or ["mcp"])],
                                    input=raw if raw is not None else (newline.join(json.dumps(m, ensure_ascii=False) for m in messages) + newline).encode(encoding),
                                    capture_output=True, env=environment, cwd=cwd or self.root, timeout=timeout)
            results.append(result)
            effects.append((self.root / "app.py").read_bytes())
        self.assertEqual(results[0].returncode, results[1].returncode, [(r.returncode, r.stderr) for r in results])
        self.assertEqual(results[0].stdout, results[1].stdout)
        self.assertEqual(results[0].stderr, results[1].stderr)
        self.assertEqual(effects[0], effects[1])
        return results[0].stdout

    def test_protocol_and_schemas(self):
        self.pair([{"id": 1, "method": "initialize", "params": {}},
                   {"id": 2, "method": "initialize", "params": {"protocolVersion": "2024-11-05"}},
                   {"method": "notifications/initialized"}, {"id": None, "method": "ping"},
                   {"id": 0, "method": "tools/list"}, {"id": "ping", "method": "ping"},
                   {"id": 3, "method": "resources/templates/list"}, {"id": 4, "method": "unknown"},
                   {"id": 5}, {"id": 6, "method": "initialize"},
                   {"id": 7, "method": "resources/read", "params": {"uri": "other://rule/x"}}])

    def test_initialize_params_type_errors(self):
        for params, kind in [(42, "int"), (1.5, "float"), (None, "NoneType"),
                             (True, "bool"), ([], "list"), ("params", "str")]:
            with self.subTest(params=params):
                replies = self.pair([{"id": 1, "method": "initialize", "params": params},
                                     {"id": 2, "method": "ping"}])
                self.assertEqual(json.loads(replies.splitlines()[0])["error"],
                                 {"code": -32000, "message": f"'{kind}' object has no attribute 'get'"})

    def test_tool_name_type_errors(self):
        for name, kind in [(42, "int"), (1.5, "float"), (None, "NoneType"),
                           (True, "bool"), ([], "list"), ({}, "dict")]:
            with self.subTest(name=name):
                replies = self.pair([{"id": 1, "method": "tools/call", "params": {"name": name}},
                                     {"id": 2, "method": "ping"}])
                error = (f"unhashable type: '{kind}'" if isinstance(name, (list, dict)) else
                         f'can only concatenate str (not "{kind}") to str')
                self.assertEqual(json.loads(replies.splitlines()[0])["result"],
                                 {"content": [{"type": "text", "text": "error: " + error}], "isError": True})

    def raw_lsp_coordinates(self, token, position, coordinate, fallback=False):
        rg = {"start": {"line": 0, "character": 4}, "end": {"line": 1, "character": 15}}
        rg[position][coordinate] = "RAW_NUMBER"
        symbol = {"name": "greet", "kind": 12, "range": rg}
        location = {"uri": oracle.uri(self.root / "app.py"), "range": rg}
        values = {"textDocument/documentSymbol": [symbol],
                  "textDocument/definition": [] if fallback else [location],
                  "textDocument/references": [] if fallback else [location],
                  "textDocument/documentHighlight": [{"range": rg}] if fallback else []}
        self.configuration["lsp"] = {"raw": {
            method: json.dumps(value).replace('"RAW_NUMBER"', token) for method, value in values.items()}}
        self.write_config()

    def test_lsp_nonfinite_character(self):
        for token in ["Infinity", "1e9999"]:
            with self.subTest(token=token):
                self.raw_lsp_coordinates(token, "start", "character")
                replies = self.pair([request("code_overview", path="app.py"),
                                     request("code_definition", path="app.py", line=1, col=5),
                                     request("code_references", path="app.py", line=1, col=5),
                                     {"id": 1, "method": "ping"}])
                for reply in replies.splitlines()[:3]:
                    result = json.loads(reply)["result"]
                    self.assertNotIn("isError", result)
                    self.assertIn('"col": Infinity', result["content"][0]["text"])

    def test_lsp_returned_extreme_coordinates(self):
        for token in [str(2**63), str(-(2**63)-1), "1e9999", "-Infinity", "NaN"]:
            for position in ["start", "end"]:
                for coordinate in ["line", "character"]:
                    for fallback in [False, True]:
                        with self.subTest(token=token, position=position, coordinate=coordinate, fallback=fallback):
                            self.raw_lsp_coordinates(token, position, coordinate, fallback)
                            self.pair([request("code_overview", path="app.py"),
                                       request("code_find", name="greet", path="app.py"),
                                       request("code_definition", path="app.py", line=1, col=5),
                                       request("code_references", path="app.py", line=1, col=5),
                                       request("code_replace_body", path="app.py", name="greet", body="replacement"),
                                       {"id": 1, "method": "ping"}],
                                      reset=lambda: (self.root / "app.py").write_text(self.source, encoding="utf-8"))

    def test_query_nonfinite_lsp_coordinates(self):
        for token in ["Infinity", "1e9999", "-Infinity", "NaN"]:
            for coordinate in ["line", "character"]:
                with self.subTest(token=token, coordinate=coordinate):
                    self.raw_lsp_coordinates(token, "start", coordinate)
                    replies = self.pair([request("query", q="greet", path="app.py"),
                                         {"id": 1, "method": "ping"}])
                    result = json.loads(replies.splitlines()[0])["result"]
                    self.assertNotIn("isError", result)
                    self.assertEqual(len(json.loads(result["content"][0]["text"])["code"]), 1)

    def test_nan_declaration_with_finite_references(self):
        for coordinate in ["line", "character"]:
            for highlights in [[], [{"range": {"start": {"line": 0, "character": 4}}}]]:
                with self.subTest(coordinate=coordinate, highlights=highlights):
                    self.raw_lsp_coordinates("NaN", "start", coordinate, fallback=True)
                    self.configuration["lsp"]["raw"]["textDocument/documentHighlight"] = json.dumps(highlights)
                    self.write_config()
                    replies = self.pair([request("code_references", path="app.py", line=1, col=5),
                                         {"id": 1, "method": "ping"}])
                    result = json.loads(replies.splitlines()[0])["result"]
                    self.assertNotIn("isError", result)
                    references = json.loads(result["content"][0]["text"])
                    self.assertEqual([(item["line"], item["col"]) for item in references],
                                     [(1, 5)] if highlights else [(1, 5), (4, 7)])

    def test_lsp_nonfinite_server_request_ids(self):
        self.configuration["lsp"] = {"requests": [
            json.dumps({"jsonrpc": "2.0", "id": value, "method": "workspace/configuration",
                        "params": {"items": [{}, {}]}})
            for value in [float("nan"), float("inf"), -float("inf")]]}
        self.write_config()
        replies = self.pair([request("code_overview", path="app.py"), {"id": 1, "method": "ping"}], timeout=10)
        self.assertNotIn("isError", json.loads(replies.splitlines()[0])["result"])

    def test_nonfinite_external_json_values(self):
        folder=self.root/"tools/decisions"
        folder.mkdir(parents=True)
        (folder/"decide.py").write_text("def decide(kind, question, input, options=None):\n    return dict(input=input,options=options)\n",encoding="utf-8")
        for number in [float("nan"),float("inf"),-float("inf")]:
            with self.subTest(number=number):
                self.configuration["kiwi"]={"out":json.dumps({"body":number})}
                self.write_config()
                self.pair([{"id":1,"method":"initialize","params":{"protocolVersion":number}},
                           {"id":1,"method":number},request("decide",kind="choice",question="q",input=number,options=[number]),
                           {"id":1,"method":"resources/read","params":{"uri":"kiwi://rule/x"}},
                           {"id":1,"method":"ping"}])
                wire="\n".join(json.dumps(message) for message in [{"id":number,"method":"ping"},{"id":[number],"method":"unknown"},{"id":1,"method":"ping"}])+"\n"
                self.pair([],raw=wire.encode("utf-8"))

    def test_combined_tool_requests_match(self):
        self.pair([request("query", q="greet", subject="kiwi"), request("code_overview", path="app.py"),
                   request("code_find", name="GREET"), request("code_definition", path="app.py", line=4, col=7),
                   request("code_references", path="app.py", line=1, col=5),
                   request("code_replace_body", path="app.py", name="greet", body="def greet(name):\n    return 42"),
                   request("ops_graph", args=["rules", "--repo", "somewhere"]),
                   request("decide", kind="choice", question="어느 쪽?", input="키위", options=["a", "b"]),
                   request("context_for", principal="bot", task="rewrite", actions=["code", "test"], paths=["a", "b"], budget=83, scope="scope", task_id="task"),
                   request("kiwi_rule", id="must"), request("kiwi_status"),
                   request("task_guard", task_id="task", fencing_token=42), request("docs_search", query="Kiwi", k=2)],
                  reset=lambda: (self.root / "app.py").write_text(self.source, encoding="utf-8"))

    def test_missing_and_unexpected_arguments_every_tool(self):
        messages = []
        for name, (_, _, required) in oracle.TOOLS.items():
            messages.append(request(name))
            messages.append(request(name, unexpected=True))
            for key in required:
                messages.append(request(name, **{other: "x" for other in required if key != other}))
        messages.extend([request("not_a_tool"), {"id": 1, "method": "tools/call", "params": {}},
                         {"id": 2, "method": "tools/call"}])
        self.pair(messages)

    def test_code_failures(self):
        messages = []
        for path in ["../escape.py", ".git/config.py"]:
            messages.extend([request("code_overview", path=path), request("code_find", name="x", path=path),
                             request("code_definition", path=path, line=1, col=1), request("code_references", path=path, line=1, col=1),
                             request("code_replace_body", path=path, name="x", body="x")])
        messages.extend([request("code_overview", path="unknown.txt"), request("code_overview", path="missing.py"),
                         request("code_replace_body", path="app.py", name="missing", body=""),
                         request("code_definition", path="app.py", line=-1, col=0), request("code_references", path="app.py", line=99, col=1)])
        self.pair(messages)

    def test_missing_servers_and_empty_docs(self):
        env = {"PATH": "", "KIWI_DOCS_SEARCH": ""}
        self.pair([request("code_overview", path="app.py"),request("code_find", name="greet"),
                   request("code_definition", path="app.py", line=4, col=7),request("code_references", path="app.py", line=1, col=5),
                   request("docs_search", query="x"),request("query", q="x")],env=env)

    def test_graph_commands_limits_and_failures(self):
        self.configuration["graph"] = {"exit": 7, "out": "앞" * 20003 + "\r\nend\n", "err": "뒤" * 2003}
        self.write_config()
        self.pair([request("ops_graph", args=[command]) for command in ["rules", "trace", "owners", "preflight", "validate", "build", "write", ""]] + [request("ops_graph", args=[])])
        self.pair([request("ops_graph", args=["rules"]),request("query", q="x", subject="kiwi")],env={"KIWI_GRAPH_BIN": str(self.base / "missing")})

    def test_kiwi_exit_codes_and_errors(self):
        messages = [request("context_for", principal="bot", task="task"), request("kiwi_rule", id="must"),request("kiwi_status"),request("task_guard", task_id="task", fencing_token=0)]
        for code in [0, 9, 10, 11, 2]:
            with self.subTest(code=code):
                self.configuration["kiwi"] = {"exit": code, "err": "  오류 " + "x" * 450 + "  "}
                self.write_config()
                self.pair(messages)
        self.configuration["kiwi"] = {"out": "", "exit": 2}
        self.write_config()
        self.pair(messages)
        self.configuration["kiwi"] = {"out": ""}
        self.write_config()
        self.pair(messages)
        self.pair(messages, env={"KIWI_BIN": str(self.base / "missing")})
        self.pair([request("kiwi_rule", id="accounts/key"),request("kiwi_rule", id=".env"),request("kiwi_rule", id="x.pem"),request("kiwi_rule", id=".git/config")])

    def test_sensitive_paths_match_approved_main(self):
        paths = ["accounts/key.md","secrets/key.md",".secrets/key.md","secret/key.md","sales/accounts/polar.md",
                 ".env",".env.local","key.key","cert.pem","config.toml","notes.txt","ignored/file.md",".hidden/note.md"]
        for path in paths+[".git/config.md"]:
            target = self.root/path
            target.parent.mkdir(parents=True,exist_ok=True)
            target.write_text("PARITY_SENSITIVE_TOKEN\n",encoding="utf-8")
        (self.root/".gitignore").write_text("ignored/\n*.key\n*.pem\n.env*\n",encoding="utf-8")
        (self.root/"accounts/app.py").write_text(self.source,encoding="utf-8")
        self.pair([request("code_overview",path="accounts/app.py"),request("code_find",name="greet",path="accounts"),
                   request("code_overview",path=".git/config.py")])
        self.configuration["docs"]={"out":"\n".join(path+": PARITY_SENSITIVE_TOKEN" for path in paths+[".git/config.md"])+"\n"}
        self.write_config()
        output=self.pair([request("docs_search",query="PARITY_SENSITIVE_TOKEN",k=50)])
        semantic=json.loads(json.loads(output)["result"]["content"][0]["text"])["out"]
        for path in paths:self.assertIn(path,semantic)
        self.assertNotIn(".git/config",semantic)
        output=self.pair([request("docs_search",query="PARITY_SENSITIVE_TOKEN",k=50)],env={"KIWI_DOCS_SEARCH":"","KIWI_ROOT":"."})
        fallback=json.loads(json.loads(output)["result"]["content"][0]["text"])["out"].replace("\\","/")
        for path in paths:self.assertIn(path,fallback)
        self.assertNotIn(".git/config",fallback)

    def test_rg_fallback_uses_approved_arguments(self):
        os.link(self.fixture,self.bin/("rg"+self.suffix))
        record=self.base/"commands.jsonl"
        self.configuration.update(record=str(record),kiwi={"out":"rules/ok.md: TOKEN\n"})
        self.write_config()
        self.pair([request("docs_search",query="TOKEN",k=2)],env={"KIWI_DOCS_SEARCH":"","KIWI_ROOT":"."})
        calls=[json.loads(line) for line in record.read_text(encoding="utf-8").splitlines()]
        expected=["-n","-F","-i","-m","3","--hidden","--no-ignore","-g","*.md","-g","*.toml","-g","*.txt", "-g",".env*","-g","*.key","-g","*.pem","-g","!.git","--","TOKEN","."]
        self.assertEqual([call["args"] for call in calls],[expected,expected])

    def test_docs_filter_and_unicode_limits(self):
        self.configuration["docs"] = {"exit": 3, "out": "accounts/key.md: secret\nx/secrets/key.md: secret\n.env.md: secret\nx.pem: secret\nrules/ok.md: " + "🌱" * 20010 + "\r\n"}
        self.write_config()
        self.pair([request("docs_search", query="키위", k=0),request("docs_search", query="x", k=-1),request("query", q="x")])

    def test_resources(self):
        with closing(sqlite3.connect(self.env["KIWI_DB"])) as db:
            db.execute("CREATE TABLE nodes(id TEXT, title TEXT, summary TEXT, path TEXT)")
            db.executemany("INSERT INTO nodes VALUES(?,?,?,?)", [("z", None, None, "rules/z.md"), ("must", "필수", "🌱" * 220, "rules/must.md"), ("account", "", "", "accounts/k.md"),("git", "", "", ".git/config")])
            db.commit()
        self.configuration["kiwi"] = {"json": {"body": "# 필수\n본문\n"}}
        self.write_config()
        self.pair([{"id": 1,"method": "resources/list"},{"id": 2,"method": "resources/read","params": {"uri": "kiwi://rule/must"}},
                   {"id": 3,"method": "resources/read","params": {"uri": "kiwi://rule/accounts/k"}}])
        self.pair([{"id": 1, "method": "resources/list"}],env={"KIWI_DB": str(self.base / "missing")})

    def test_decisions_and_adapter(self):
        folder = self.root / "tools/decisions"
        folder.mkdir(parents=True)
        (folder / "decide.py").write_text("def decide(kind, question, input, options=None):\n    if kind == 'exit': raise SystemExit('not configured')\n    if kind == 'error': raise ValueError('synthetic failure')\n    return dict(kind=kind, question=question, input=input, options=options, unicode='키위')\n", encoding="utf-8")
        (folder / "adapter.py").write_text("def brief(args):\n    if 'fail' in args: raise ValueError('fallback')\n    return dict(adapter=args, unicode='키위')\n", encoding="utf-8")
        self.pair([request("decide", kind="predicate", question="q", input="i"),request("decide", kind="score",question="q",input="i",options=["low","high"]),
                   request("decide", kind="exit", question="q", input="i"),request("decide", kind="error", question="q", input="i"),
                   request("context_for",principal="bot",task="task"),request("context_for",principal="bot",task="fail")],env={"KIWI_DECISIONS":"on"})

    def test_ask_and_resolve_bridge_contract(self):
        folder = self.root / "tools/decisions"
        folder.mkdir(parents=True)
        (folder / "ask.py").write_text(
            "def ask(question, options=None, task='', principal=''):\n"
            "    if question == 'error': raise ValueError('synthetic ask failure')\n"
            "    if question == 'exit': raise SystemExit('not configured')\n"
            "    risky = question == 'pay'\n"
            "    return dict(ask_id='ask-fixture', status='needs_user' if risky else 'answered', "
            "answer='오너 확인 필요' if risky else '키위', confidence=0.0 if risky else 0.9, "
            "evidence=[], options=options, task=task, principal=principal)\n"
            "def resolve(ask_id, actual):\n"
            "    if ask_id == 'missing': raise ValueError('unknown ask missing')\n"
            "    if ask_id == 'exit': raise SystemExit('not configured')\n"
            "    return dict(ask_id=ask_id, actual=actual, hit=True)\n", encoding="utf-8")
        output = self.pair([
            {"id": 1, "method": "tools/list"}, request("ask", question="로그인"),
            request("ask", question="선택", options=["a", "b"], task="작업", principal="bot"),
            request("ask", question="pay"), request("ask", question="error"),
            request("ask_resolve", ask_id="ask-fixture", actual="a"),
            request("ask_resolve", ask_id="missing", actual="a"),
            request("ask", question="exit"), request("ask_resolve", ask_id="exit", actual="a"),
            request("ask", question="로그인", threshold=0), request("ask", question="로그인", kiwi="elsewhere"),
            request("ask_resolve", ask_id="ask-fixture", actual="a", threshold=0),
            request("ask_resolve", ask_id="ask-fixture", actual="a", kiwi="elsewhere"),
            request("ask", question="로그인")])
        replies = [json.loads(line) for line in output.splitlines()]
        tools = {tool["name"]: tool for tool in replies[0]["result"]["tools"]}
        self.assertEqual(tools["ask"]["inputSchema"]["required"], ["question"])
        self.assertEqual(tools["ask_resolve"]["inputSchema"]["required"], ["ask_id", "actual"])
        self.assertEqual(json.loads(replies[3]["result"]["content"][0]["text"])["answer"], "오너 확인 필요")
        self.assertTrue(replies[4]["result"]["isError"])
        self.assertTrue(json.loads(replies[5]["result"]["content"][0]["text"])["hit"])
        for reply in replies[7:13]:
            self.assertTrue(reply["result"]["isError"])
        for reply in replies[7:9]:
            self.assertIn("Decisions not configured on this machine (not configured)", reply["result"]["content"][0]["text"])
        self.assertFalse(replies[-1]["result"].get("isError", False))

    def test_ask_stats_is_read_only_and_matches_both_runtimes(self):
        folder = self.root / "tools/decisions"
        folder.mkdir(parents=True)
        (folder / "ask.py").write_text(
            "def stats(days=30, write=None):\n"
            "    if write is not None: raise AssertionError('write reached stats')\n"
            "    if type(days) is not int or not 1 <= days <= 3650: raise ValueError('days must be a positive integer')\n"
            "    if days == 17: raise SystemExit('not configured')\n"
            "    return dict(days=days, total=5, answered=3, needs_user=2, proxy_rate=0.6)\n", encoding="utf-8")
        report = self.root / "report.md"
        output = self.pair([
            {"id": 1, "method": "tools/list"}, request("ask_stats"), request("ask_stats", days=7),
            request("ask_stats", write=str(report)), request("ask_stats", days=7, write=str(report)),
            request("ask_stats", unknown=True), request("ask_stats", days=0), request("ask_stats", days=True),
            request("ask_stats", days="7"), request("ask_stats", days=None), request("ask_stats", days=17),
            request("ask_stats", days=1), {"id": 1, "method": "ping"}])
        replies = [json.loads(line)["result"] for line in output.splitlines()]
        schema = next(tool["inputSchema"] for tool in replies[0]["tools"] if tool["name"] == "ask_stats")
        self.assertEqual(schema, {"type": "object", "properties": {"days": {"type": "integer"}}, "required": []})
        self.assertEqual(json.loads(replies[1]["content"][0]["text"])["days"], 30)
        self.assertEqual(json.loads(replies[2]["content"][0]["text"])["days"], 7)
        for reply in replies[3:11]:
            self.assertTrue(reply["isError"])
        self.assertEqual(replies[3]["content"][0]["text"], "error: Sprouts.ask_stats() got an unexpected keyword argument 'write'")
        self.assertIn("Decisions not configured on this machine (not configured)", replies[10]["content"][0]["text"])
        self.assertEqual(json.loads(replies[11]["content"][0]["text"])["days"], 1)
        self.assertFalse(report.exists())

    def test_context_question_preserves_bundle_and_contains_ask_failures(self):
        folder = self.root / "tools/decisions"
        folder.mkdir(parents=True)
        bundle = {"bundle_id": "fixture", "mandatory_rules": [{"id": "must"}], "cache": "hit"}
        self.configuration["kiwi"] = {"json": bundle}
        self.write_config()
        (folder / "adapter.py").write_text("def brief(args):\n    return " + repr(bundle) + "\n", encoding="utf-8")
        (folder / "ask.py").write_text(
            "import subprocess\n"
            "def ask(question, options=None, task='', principal=''):\n"
            "    if question == 'error': raise ValueError('synthetic ask failure')\n"
            "    if question == 'exit': raise SystemExit('not configured')\n"
            "    if question == 'timeout': raise subprocess.TimeoutExpired('kiwi-ask', 90)\n"
            "    if question == 'missing': raise FileNotFoundError('kiwi-ask missing')\n"
            "    return dict(status='answered', answer=question, options=options, task=task, principal=principal)\n", encoding="utf-8")
        for decisions in ("off", "on"):
            with self.subTest(decisions=decisions):
                output = self.pair([
                    request("context_for", principal="bot", task="task"),
                    request("context_for", principal="bot", task="task", question=None, options=["unused"]),
                    request("context_for", principal="bot", task="task", options=["unused"]),
                    *[request("context_for", principal="bot", task="task", question=question, options=["a", "b"])
                      for question in ("choose", "", "error", "exit", "timeout", "missing")],
                    request("context_for", principal="bot", task="task", question="fresh"),
                    request("context_for", principal="bot", task="task"), {"id": 1, "method": "ping"}],
                    env={"KIWI_DECISIONS": decisions})
                bundles = [json.loads(json.loads(line)["result"]["content"][0]["text"]) for line in output.splitlines()[:-1]]
                for index in (0, 1, 2, 10):
                    self.assertEqual(bundles[index], bundle)
                for index, question in ((3, "choose"), (4, ""), (9, "fresh")):
                    self.assertEqual(bundles[index]["answer"], dict(status="answered", answer=question,
                                     options=None if index == 9 else ["a", "b"], task="task", principal="bot"))
                for item in bundles[5:9]:
                    self.assertEqual({key: value for key, value in item.items() if key != "answer"}, bundle)
                    self.assertEqual(item["answer"]["status"], "needs_user")
                    self.assertEqual(item["answer"]["answer"], "오너 확인 필요")
                    self.assertEqual(item["answer"]["reason"], "error")
                    self.assertTrue(item["answer"]["error"])

    def test_context_missing_ask_module_preserves_bundle_in_both_runtimes(self):
        bundle = {"bundle_id": "fixture", "mandatory_rules": [{"id": "must"}]}
        self.configuration["kiwi"] = {"json": bundle}
        self.write_config()
        output = self.pair([request("context_for", principal="bot", task="task", question="q"),
                            request("context_for", principal="bot", task="task"), {"id": 1, "method": "ping"}])
        replies = [json.loads(line)["result"] for line in output.splitlines()]
        answered = json.loads(replies[0]["content"][0]["text"])
        self.assertEqual(answered, {**bundle, "answer": {"status": "needs_user", "answer": "오너 확인 필요", "reason": "error", "error": "No module named 'ask'"}})
        self.assertEqual(json.loads(replies[1]["content"][0]["text"]), bundle)
        self.assertEqual(replies[2], {})

    def test_real_ask_without_key_preserves_evidence_in_both_runtimes(self):
        folder = self.root / "tools/decisions"
        folder.mkdir(parents=True)
        source = HERE.parents[2] / "tools/decisions"
        for name in ("ask.py", "gate.py", "adapter.py", "proxy.py", "semantic.py", "synonyms.json"):
            shutil.copyfile(source / name, folder / ("ask_impl.py" if name == "ask.py" else name))
        (folder / "decide.py").write_text(
            "MODEL='fake'\ndef api_key():\n    raise SystemExit('not configured')\n"
            "def decide(*a, **k):\n    api_key()\n", encoding="utf-8")
        (folder / "ask.py").write_text(
            "from ask_impl import ask, resolve\nimport time\ntime.time_ns=lambda: 123\ntime.time=lambda: 123\n", encoding="utf-8")
        (self.root / "rules").mkdir()
        (self.root / "rules/login.md").write_text("# Login\nBots handle login autonomously.", encoding="utf-8")
        home = self.base / "ask-home"
        home.mkdir()
        def reset():
            (home / "ops.db").unlink(missing_ok=True)
        output = self.pair([request("ask", question="who handles login"),
                            request("ask", question="login", options=["autonomous", "approval"]),
                            {"id": 3, "method": "ping"}],
                           env={"KIWI_HOME": str(home), "KIWI_ASK_SEMANTIC": "off"}, reset=reset)
        replies = [json.loads(line) for line in output.splitlines()]
        for reply in replies[:2]:
            self.assertFalse(reply["result"].get("isError", False))
            result = json.loads(reply["result"]["content"][0]["text"])
            self.assertEqual((result["status"], result["answer"]), ("needs_user", "오너 확인 필요"))
            self.assertEqual(result["evidence"][0]["id"], "rules/login")
        self.assertEqual(replies[-1]["result"], {})

    def test_adapter_system_exit(self):
        folder=self.root/"tools/decisions"
        folder.mkdir(parents=True)
        (folder/"adapter.py").write_text("def brief(args):\n    raise SystemExit('stop')\n",encoding="utf-8")
        self.pair([request("context_for",principal="bot",task="task"),{"id":1,"method":"ping"}],env={"KIWI_DECISIONS":"on"})

    def test_lsp_location_links_highlights_and_retries(self):
        rg = {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 9}}
        self.configuration["lsp"] = {"definition": [{"targetUri":"$DOCUMENT","targetSelectionRange":rg}],"references":[],"highlights":[{"range":rg},{"range":{"start":{"line":3,"character":6},"end":{"line":3,"character":11}}}]}
        self.write_config()
        self.pair([request("code_definition",path="app.py",line=4,col=7),request("code_references",path="app.py",line=1,col=5)])
        (self.root / "app.rs").write_text(self.source,encoding="utf-8")
        self.configuration["lsp"]["retry"] = 1
        self.configuration["lsp"]["references"] = [{"uri":"$DOCUMENT","range":rg}]
        self.write_config()
        self.pair([request("code_definition",path="app.rs",line=4,col=7),request("code_references",path="app.rs",line=1,col=5)])

    def test_cli_and_root_flag(self):
        for args in [["kiwi_status"], ["--root",str(self.root),"code_find",'{"name":"greet"}'],["docs_search",'{"query":"Kiwi","k":2}']]:
            self.pair([],cli=args)

    def test_cli_error_tracebacks(self):
        for args in [["unknown"],["code_overview"],["--root"],["kiwi_status","invalid"]]:
            with self.subTest(args=args):
                self.pair([],cli=args)

    def test_health_absolute_test_path(self):
        self.configuration["suite"] = True
        self.write_config()
        environment = dict(self.env)
        environment.pop("KIWI_SPROUTS_BIN",None)
        prefixes = []
        for command in [[sys.executable,str(HERE/"kiwi_sprouts.py")],[BINARY]]:
            with self.subTest(command=command):
                result = subprocess.run([*command,"--health"], env=environment, cwd=self.root,
                                        capture_output=True, timeout=90)
                self.assertEqual(result.returncode,0,result.stderr)
                self.assertEqual(result.stdout,b"")
                self.assertRegex(result.stderr, rb"Ran [1-9][0-9]* tests?")
                self.assertTrue(result.stderr.endswith(b"OK\r\n") or result.stderr.endswith(b"OK\n"),result.stderr)
                prefixes.append(result.stderr.splitlines(keepends=True)[0])
        self.assertEqual(prefixes[0],prefixes[1])

    def test_argument_shapes(self):
        self.pair([{"id":1,"method":"tools/call","params":{"name":"kiwi_status","arguments":value}} for value in [None,False,0,"",[],{},[1],"x",True,1]])

    def test_nested_symbols_swift_and_duplicate_replace(self):
        rg={"start":{"line":0,"character":0},"end":{"line":1,"character":15}}
        selection={"start":{"line":0,"character":4},"end":{"line":0,"character":9}}
        self.configuration["lsp"]={"symbols":[{"name":"Box", "kind":5,"range":rg,"children":[{"name":"greet", "kind":12,"range":rg,"selectionRange":selection}]},{"name":"greet", "kind":99,"location":{"range":rg}}]}
        self.write_config()
        self.pair([request("code_overview",path="app.py"),request("code_find",name="greet"),request("code_replace_body",path="app.py",name="greet",body="x")])
        (self.root/"app.swift").write_text("func triple(_ x: Int) -> Int {}",encoding="utf-8")
        self.configuration["lsp"]={"symbols":[{"name":"triple<T>(_:)","kind":12,"range":rg}]}
        self.write_config()
        self.pair([request("code_overview",path="app.swift"),request("code_find",name="TRIPLE",path="app.swift")])

    def test_regression_malformed_document_symbols(self):
        valid_range = {"start":{"line":0,"character":4},"end":{"line":1,"character":15}}
        symbols = [
            {"kind":12,"range":valid_range},
            *[{"name":name,"kind":12,"range":valid_range} for name in [None,42,[],{}]],
            {"name":"greet","kind":12,"range":{"start":{"character":4},"end":{"line":1,"character":15}}},
            {"name":"greet","kind":12,"range":{"start":{"line":0,"character":4},"end":{"character":15}}},
            {"name":"greet","range":valid_range},
            *[{"name":"greet","kind":12,"range":valid_range,"children":children} for children in [{"name":"nested"},1]],
            {"name":"greet","kind":12,"range":{"start":{"line":0,"character":4},"end":{"line":None,"character":0}}},
            *[{"name":"greet","kind":12,"range":{"start":{"line":0,"character":4},"end":{"line":1,"character":character}}}
              for character in [1.5,"x",None,[],{}]],
        ]
        for response in [[symbol] for symbol in symbols]+[{"name":"greet"},"greet",1]:
            with self.subTest(response=response):
                self.configuration["lsp"] = {"symbols":response}
                self.write_config()
                self.pair([request("code_overview",path="app.py"),request("code_find",name="greet",path="app.py"),
                           request("code_definition",path="app.py",line=1,col=5),request("code_references",path="app.py",line=1,col=5),
                           request("code_replace_body",path="app.py",name="greet",body="replacement"),{"id":1,"method":"ping"}],
                          reset=lambda:(self.root/"app.py").write_text(self.source,encoding="utf-8"))

    def test_rg_fallback(self):
        if not shutil.which("rg"):
            self.skipTest("rg missing")
        (self.root/"rules").mkdir()
        (self.root/"rules/ok.md").write_text("Kiwi\nKiwi 2\nKiwi 3\nKiwi 4\n",encoding="utf-8")
        (self.root/"accounts").mkdir()
        (self.root/"accounts/key.md").write_text("Kiwi",encoding="utf-8")
        for k in [0,1,5,-1]:
            self.pair([request("docs_search",query="kiwi",k=k)],env={"KIWI_DOCS_SEARCH":"","KIWI_ROOT":"."})
        self.pair([request("docs_search",query="kiwi")],env={"KIWI_DOCS_SEARCH":""})

    def test_resource_database_errors(self):
        Path(self.env["KIWI_DB"]).write_bytes(b"invalid database")
        self.pair([{"id":1,"method":"resources/list"}])
        Path(self.env["KIWI_DB"]).unlink()
        with closing(sqlite3.connect(self.env["KIWI_DB"])) as db:
            db.execute("CREATE TABLE unrelated(id TEXT)")
        self.pair([{"id":1,"method":"resources/list"}])

    def test_regression_integer_boundaries(self):
        values=[-(2**128),-(2**63),2**63-1,2**63,2**128]
        with self.subTest(backend="semantic"):
            self.pair([request("docs_search",query="x",k=value) for value in values])
        with self.subTest(backend="rg"):
            self.pair([request("docs_search",query="absent",k=value) for value in values]+[{"id":1,"method":"ping"}],env={"KIWI_DOCS_SEARCH":"","KIWI_ROOT":"."})
        for tool in ["code_definition","code_references"]:
            with self.subTest(tool=tool):
                self.pair([request(tool,path="app.py",line=-(2**63),col=1),request(tool,path="app.py",line=4,col=-(2**63)),
                           request(tool,path="app.py",line=2**128,col=2**128),{"id":1,"method":"ping"}])

    @unittest.skipUnless(os.name=="nt","Windows ANSI code page regression")
    def test_regression_cp949_child_output(self):
        output=self.base/"cp949.txt"
        output.write_bytes("rules/ok.md: 한글\r\n".encode("cp949"))
        tool=self.base/"cp949.cmd"
        tool.write_text(f'@echo off\r\ntype "{output}"\r\n',encoding="ascii",newline="")
        self.pair([request("docs_search",query="x")],env={"KIWI_DOCS_SEARCH":str(tool),"PYTHONUTF8":"0"})
        self.pair([request("docs_search",query="x")],env={"KIWI_DOCS_SEARCH":str(tool),"PYTHONUTF8":"0","PYTHONIOENCODING":""})

    @unittest.skipUnless(os.name=="nt","Windows strict text decoding")
    def test_windows_strict_child_decode_error(self):
        self.configuration["docs"]={"out":"rules/ok.md: 키위 🌱\n"}
        self.write_config()
        self.pair([request("docs_search",query="x"),{"id":1,"method":"ping"}],env={"PYTHONUTF8":"0"})

    @unittest.skipUnless(os.name=="nt","Windows stdout encoding")
    def test_windows_stdout_encode_error(self):
        self.pair([request("docs_search",query="x")],env={"PYTHONIOENCODING":"cp949"})

    def test_explicit_stdio_codecs(self):
        for encoding in ["cp949:replace","cp949:backslashreplace","utf-16"]:
            with self.subTest(encoding=encoding):
                self.pair([request("docs_search",query="x"),{"id":1,"method":"ping"}],env={"PYTHONIOENCODING":encoding})
        if os.name=="nt":
            self.pair([request("docs_search",query="x")],env={"PYTHONIOENCODING":":replace","PYTHONUTF8":"0"})

    def test_universal_newlines(self):
        for newline in ["\n","\r\n"] + (["\r"] if os.name=="nt" else []):
            with self.subTest(newline=newline):
                self.pair([{"id":1,"method":"ping"},{"id":2,"method":"ping"}],newline=newline)

    @unittest.skipIf(os.name=="nt","POSIX-only intended difference")
    def test_intended_difference_bare_cr_on_posix(self):
        # Intended difference (오너 2026-10-23 18:50): on POSIX, Python reads a bare-CR stream as one line and
        # crashes with JSONDecodeError (exit 1). Rust splits on CR like Windows/universal newlines and answers both.
        raw=b'{"id":1,"method":"ping"}\r{"id":2,"method":"ping"}\r'
        result=subprocess.run([BINARY,"mcp"],input=raw,capture_output=True,env=self.env,cwd=self.root,timeout=30)
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertEqual([json.loads(line)["id"] for line in result.stdout.splitlines()],[1,2])
        self.assertEqual(result.stderr,b"")

    def test_malformed_mcp_input(self):
        for raw in [b'invalid\n',b'[]\n',b'null\n',b'{"id":1,"method":"ping"}\n{\n',b'\x1c\n{"id":1,"method":"ping"}\n']:
            with self.subTest(raw=raw):
                self.pair([],raw=raw)

    def test_interactive_stdio_before_eof(self):
        for encoding in ["utf-8","utf-16"]:
            wire=[]
            for command in [[sys.executable,str(HERE/"kiwi_sprouts.py")],[BINARY]]:
                process=subprocess.Popen([*command,"mcp"],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,
                                         env={**self.env,"PYTHONIOENCODING":encoding},cwd=self.root)
                encoder=codecs.getincrementalencoder(encoding)()
                decoder=codecs.getincrementaldecoder(encoding)()
                replies=[]
                try:
                    for identifier in [1,2]:
                        process.stdin.write(encoder.encode(json.dumps({"id":identifier,"method":"ping"})+"\n"))
                        process.stdin.flush()
                        raw=bytearray()
                        def read_reply():
                            while True:
                                chunk=process.stdout.read(1)
                                if not chunk:break
                                raw.extend(chunk)
                                if "\n" in decoder.decode(chunk):break
                        reader=threading.Thread(target=read_reply,daemon=True)
                        reader.start();reader.join(timeout=10)
                        self.assertFalse(reader.is_alive(),"server waited for EOF before replying")
                        replies.append(bytes(raw))
                    process.stdin.close()
                    self.assertEqual(process.wait(timeout=10),0)
                    self.assertEqual(process.stderr.read(),b"")
                    wire.append(replies)
                finally:
                    if process.poll() is None:process.kill();process.wait()
                    for stream in [process.stdin,process.stdout,process.stderr]:stream.close()
            self.assertEqual(wire[0],wire[1])

    def test_json_backend_errors_and_constants(self):
        for text in ["", "invalid", "{", '{"x":1,}', ' {"x":1} trailing', '{"x":NaN,"y":Infinity,"z":-Infinity}', '{"x":1e9999}']:
            with self.subTest(text=text):
                self.configuration["kiwi"]={"out":text}
                self.write_config()
                self.pair([request("kiwi_status"),{"id":1,"method":"ping"}])
        self.pair([{"id":1,"method":"모르는 메서드"},{"id":1,"method":"resources/read","params":{"uri":"unknown://키위"}}])

    @unittest.skipUnless(os.name=="nt","Windows drive-relative path regression")
    def test_regression_drive_relative_paths(self):
        drive=self.root.drive
        self.pair([request("code_overview",path=drive+"app.py"),request("code_overview",path=drive.lower()+"app.py"),
                   request("code_find",name="greet",path=drive),request("code_overview",path=drive+"accounts/key.py")],cwd=self.base)

    def test_regression_float_formatting(self):
        self.configuration["kiwi"]={"out":'{"small":1e-6,"decimal":1.00,"big":1e20,"negative_zero":-0.0,"integer_zero":-0,"tiny":5e-324,"fraction":0.0001,"threshold":1e16,"precise":1.2345678901234567}'}
        self.write_config()
        self.pair([request("kiwi_status"),request("kiwi_rule",id="x")])
        self.pair([],cli=["kiwi_status"])
        generator=random.Random(17)
        values=[struct.unpack("!d",generator.getrandbits(64).to_bytes(8,"big"))[0] for _ in range(3000)]
        values=[value for value in values if abs(value)!=float("inf") and value==value]
        self.configuration["kiwi"]={"out":json.dumps(values)}
        self.write_config()
        self.pair([request("kiwi_status")])


def tearDownModule():
    if COVERAGE:
        print("PARITY COVERAGE " + json.dumps(dict(sorted(COVERAGE.items())), sort_keys=True), file=sys.stderr)


if __name__ == "__main__":
    unittest.main()
