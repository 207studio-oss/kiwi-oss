#!/usr/bin/env python3
"""kiwi-sprouts: one stdio MCP/CLI over Kiwi's graphs.

code  : LSP symbol graph (pyright, typescript-language-server, rust-analyzer, sourcekit-lsp)
ops   : existing kiwi-graph (Rust) read-only queries
docs  : existing brain-search (Mac, EmbeddingGemma) via kiwi-search alias; box falls back to rg
No dashboard, no network listener, no browser. stdlib only.

Usage:
  python3 kiwi_sprouts.py mcp             # stdio MCP server
  python3 kiwi_sprouts.py --health        # run the local unittest suite
  python3 kiwi_sprouts.py TOOL JSON_ARGS  # one-shot CLI call
"""
import json, os, re, shutil, subprocess, sys, threading, time
from collections import deque
from pathlib import Path
from urllib.parse import unquote
from urllib.request import url2pathname

from kiwi_brief import BriefGuard, calibrated_pass
from kiwi_events import Claims, EventBus, Tokens
from kiwi_selfimprove import SelfImprove
import kiwi_dsh_host

HERE = Path(__file__).resolve().parent
KIWI = Path(os.environ.get("KIWI_ROOT", HERE.parents[2]))
sys.path.insert(0, str(KIWI / "tools" / "decisions"))
LSP = {
    ".py": ["pyright-langserver", "--stdio"],
    ".ts": ["typescript-language-server", "--stdio"], ".tsx": ["typescript-language-server", "--stdio"],
    ".js": ["typescript-language-server", "--stdio"],
    ".rs": ["rust-analyzer"],
    ".swift": ["sourcekit-lsp"],
}
LANG_ID = {".py": "python", ".ts": "typescript", ".tsx": "typescriptreact", ".js": "javascript", ".rs": "rust", ".swift": "swift"}
SKIP = {".git", "node_modules", "target", ".build", "__pycache__", ".venv", "dist"}
# Kiwi exists so bots can read sensitive material (오너 2026-10-23 17:54 "응, 풀어"):
# accounts/, secrets, .env, *.key, *.pem are readable and searchable. Only .git internals are excluded.
DENY_DIRS = {".git"}


def denied(rel):
    """True only for .git internals; sensitive paths are intentionally readable."""
    return ".git" in Path(str(rel)).parts


def _line_denied(line):
    return any(denied(tok) for tok in re.split(r"[\s:]+", line) if "/" in tok or "." in tok)


KINDS = {5: "class", 6: "method", 11: "interface", 12: "function", 13: "variable", 14: "constant", 23: "struct", 10: "enum", 22: "enum_member", 8: "field", 2: "module", 9: "constructor", 26: "type_param", 7: "property"}


def uri(p): return Path(p).resolve().as_uri()
def unuri(u): return url2pathname(u[7:]) if u.startswith("file://") else u


class Lsp:
    def __init__(self, cmd, root, init_opts=None):
        exe = shutil.which(cmd[0])
        if not exe: raise RuntimeError(f"language server not found: {cmd[0]} (run bootstrap.sh)")
        self.cmd, self.key = cmd[0], cmd[0]
        self.p = subprocess.Popen([exe, *cmd[1:]], cwd=root, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.id, self.res, self.cv, self.opened = 0, {}, threading.Condition(), set()
        self.err, self.eof, self.closed = deque(maxlen=20), False, False
        threading.Thread(target=self._read, daemon=True).start()
        threading.Thread(target=self._drain_err, daemon=True).start()
        params = {"processId": os.getpid(), "rootUri": uri(root), "workspaceFolders": [{"uri": uri(root), "name": "root"}],
                  "capabilities": {"workspace": {"configuration": True},
                                   "textDocument": {"documentSymbol": {"hierarchicalDocumentSymbolSupport": True}}}}
        if init_opts: params["initializationOptions"] = init_opts
        try:
            self.req("initialize", params)
            self.note("initialized", {})
        except Exception:
            self.close(); raise

    def _send(self, m):
        b = json.dumps(m).encode()
        try:
            self.p.stdin.write(b"Content-Length: %d\r\n\r\n" % len(b) + b); self.p.stdin.flush()
        except (BrokenPipeError, OSError) as e:
            with self.cv: self.eof = True; self.cv.notify_all()
            raise RuntimeError(f"{self.cmd} is not running") from e

    def _read(self):
        f = self.p.stdout
        try:
            while True:
                n = 0
                while True:
                    line = f.readline()
                    if not line: return
                    if line in (b"\r\n", b"\n"): break
                    if line.lower().startswith(b"content-length:"): n = int(line.split(b":")[1])
                m = json.loads(f.read(n))
                if "id" in m and "method" in m:  # server->client request: answer minimally
                    self._send({"jsonrpc": "2.0", "id": m["id"], "result": [None] * len(m.get("params", {}).get("items", [])) if m["method"] == "workspace/configuration" else None})
                elif "id" in m:
                    with self.cv: self.res[m["id"]] = m; self.cv.notify_all()
        except Exception as e:
            with self.cv: self.err.append(f"read error: {e}"); self.eof = True; self.cv.notify_all()
        finally:
            with self.cv: self.eof = True; self.cv.notify_all()

    def _drain_err(self):
        try:
            for line in self.p.stderr:
                self.err.append(line.decode("utf-8", "replace").strip())
        except Exception:
            pass

    def note(self, method, params): self._send({"jsonrpc": "2.0", "method": method, "params": params})

    def req(self, method, params, timeout=60):
        self.id += 1; i = self.id
        self._send({"jsonrpc": "2.0", "id": i, "method": method, "params": params})
        with self.cv:
            ok = self.cv.wait_for(lambda: i in self.res or self.eof, timeout)
            if i in self.res:
                m = self.res.pop(i)
            elif not ok:
                raise TimeoutError(method)
            else:
                tail = "; ".join(self.err) or "no stderr"
                raise RuntimeError(f"{self.cmd} exited before answering {method}: {tail}")
        if "error" in m: raise RuntimeError(m["error"].get("message"))
        return m.get("result")

    def open(self, path):
        path = str(Path(path).resolve())
        if path not in self.opened:
            self.note("textDocument/didOpen", {"textDocument": {"uri": uri(path), "languageId": LANG_ID[Path(path).suffix], "version": 1,
                                                                "text": Path(path).read_text(encoding="utf-8", errors="replace")}})
            self.opened.add(path)

    # Kiwi v2: one call at task start returns only what this task needs (dev/tools/kiwi-core).
    def _kiwi(self, *args):
        exe = os.environ.get("KIWI_BIN") or shutil.which("kiwi") or str(Path.home() / ".local/bin/kiwi")
        r = subprocess.run([exe, *args], capture_output=True, text=True, timeout=120)
        if r.returncode not in (0, 9, 10, 11): raise RuntimeError("kiwi: " + (r.stderr.strip() or str(r.returncode))[:400])
        return json.loads(r.stdout)

    def context_for(self, principal, task, actions=None, paths=None, budget=4096, task_id="", scope="*"):
        args = ["--principal", principal, "--task", task, "--budget", str(budget), "--scope", scope]
        if actions: args += ["--actions", ",".join(actions)]
        if paths: args += ["--paths", ",".join(paths)]
        if task_id: args += ["--task-id", task_id]
        if os.environ.get("KIWI_DECISIONS", "on") != "off":
            try:
                import adapter
                return adapter.brief(args)
            except Exception:
                pass  # Decisions/adapter unavailable: plain bundle with local scores
        return self._kiwi("brief", *args)

    def kiwi_rule(self, id):
        if denied(id): raise ValueError("denied")
        return self._kiwi("rule", id)

    def kiwi_status(self):
        return self._kiwi("status")

    def task_guard(self, task_id, fencing_token):
        return self._kiwi("task", "guard", "--task-id", task_id, "--fencing-token", str(fencing_token))

    def close(self):
        if self.closed: return
        self.closed = True
        if self.p.poll() is None:
            try: self.req("shutdown", None, 3); self.note("exit", None)
            except Exception: pass
            try: self.p.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.p.terminate()
                try: self.p.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    self.p.kill(); self.p.wait()
        for f in (self.p.stdin, self.p.stdout, self.p.stderr):
            try: f.close()
            except Exception: pass


def tsserver_path():
    """Path to a TypeScript 5.x tsserver.js, if one is installed.

    typescript-language-server 5.x cannot use the TypeScript 7 native preview
    (it has no lib/tsserver.js), so bootstrap installs a compatible copy aside.
    """
    cands = [os.environ.get("KIWI_TSSERVER_PATH"),
             Path.home() / ".local/lib/kiwi-sprouts/typescript/node_modules/typescript/lib/tsserver.js",
             Path.home() / ".local/lib/node_modules/typescript/lib/tsserver.js"]
    for c in cands:
        if c and Path(c).is_file(): return str(c)
    return None


def clean_name(name, path):
    """Swift (and some C-like) servers include signatures: triple(_:) -> triple."""
    if Path(path).suffix == ".swift" and name.endswith(")") and "(" in name:
        name = name.split("(", 1)[0]
        if name.endswith(">") and "<" in name: name = name.split("<", 1)[0]
    return name



def _decisions_call(method, *args, **kwargs):
    try:
        return method(*args, **kwargs)
    except SystemExit as error:
        raise RuntimeError(f"Decisions not configured on this machine ({error}). Do NOT ask the user for a key in chat; report 'Decisions 미설정' and stop. Setup: Kiwi tools/windows/kiwi-v2-apply.ps1 / tools/mac/kiwi-v2-apply.sh.") from None


CONTEXT_CACHE = {}
CONTEXT_CACHE_TTL = 600


def _answer_part(result, threshold=None):
    """Turn a Decisions ask result into the context_for decision block."""
    result = result if isinstance(result, dict) else {}
    try:
        confidence = float(result.get("confidence") or 0.0)
    except (TypeError, ValueError):
        confidence = 0.0
    try:
        floor = 0.75 if threshold is None else float(threshold)
    except (TypeError, ValueError):
        floor = 0.75
    evidence = []
    for key in ("evidence", "decisions"):
        for item in result.get(key) or []:
            if isinstance(item, dict):
                if item.get("id") is not None: evidence.append(item["id"])
            elif item is not None:
                evidence.append(item)
    needs_user = None
    if result.get("status") != "answered" or confidence < floor:
        needs_user = {"reason": result.get("reason") or "decisions_unavailable"}
        if result.get("best_guess"):
            needs_user["best_guess"] = result["best_guess"]
    return {"answer": result.get("answer") if result.get("status") == "answered" else None,
            "confidence": confidence, "evidence": evidence, "needs_user": needs_user,
            "ask_id": result.get("ask_id")}


class Sprouts:
    def __init__(self, root=None, events=None):
        self.root = Path(root or os.environ.get("KIWI_SPROUTS_ROOT", os.getcwd())).resolve()
        self.servers = {}
        self.events = events or EventBus()
        self.claims = Claims(self.root, self.events)
        self.tokens = Tokens(self.root, self.events)
        self.brief = BriefGuard(self.root, self.events)
        self.selfimprove = SelfImprove(self.root, self.events, self.tokens)

    def lsp(self, path):
        ext = Path(path).suffix
        if ext not in LSP: raise ValueError(f"unsupported file type: {ext}")
        key = LSP[ext][0]
        if key not in self.servers:
            ts = tsserver_path() if key == "typescript-language-server" else None
            self.servers[key] = Lsp(LSP[ext], self.root, {"tsserver": {"path": ts}} if ts else None)
        s = self.servers[key]; s.open(path); return s

    def abs(self, p):
        q = (self.root / p).resolve()
        if self.root not in q.parents and q != self.root: raise ValueError("path outside root")
        if denied(q.relative_to(self.root)): raise ValueError("path denied")
        return q

    def _flat(self, syms, path, parent=""):
        for s in syms or []:
            name = (parent + "/" if parent else "") + clean_name(s["name"], path)
            r = s.get("range") or s["location"]["range"]
            sel = s.get("selectionRange", r)
            yield {"name": name, "kind": KINDS.get(s["kind"], s["kind"]), "path": str(Path(path).relative_to(self.root)),
                   "line": sel["start"]["line"] + 1, "col": sel["start"]["character"] + 1, "range": r}
            yield from self._flat(s.get("children"), path, name)

    # ---- code graph ----
    def code_overview(self, path):
        p = self.abs(path)
        return [{k: v for k, v in s.items() if k != "range"} for s in self._flat(self.lsp(p).req("textDocument/documentSymbol", {"textDocument": {"uri": uri(p)}}), p)]

    def _files(self, under="."):
        base = self.abs(under)
        files = [base] if base.is_file() else [f for f in base.rglob("*") if f.suffix in LSP and not (SKIP & set(f.relative_to(self.root).parts)) and not denied(f.relative_to(self.root))]
        return sorted(files)[:400]

    def code_find(self, name, path="."):
        out = []
        for f in self._files(path):
            try: syms = self.lsp(f).req("textDocument/documentSymbol", {"textDocument": {"uri": uri(f)}})
            except Exception: continue
            for s in self._flat(syms, f):
                if name.lower() in s["name"].rsplit("/", 1)[-1].lower():
                    s.pop("range"); out.append(s)
        return out[:100]

    def _locs(self, method, path, line, col, extra=None, retries=1, delay=0.5):
        p = self.abs(path)
        s = self.lsp(p)
        for attempt in range(retries):
            try:
                r = s.req(method, {"textDocument": {"uri": uri(p)}, "position": {"line": line - 1, "character": col - 1}, **(extra or {})})
            except RuntimeError as e:
                msg = str(e).lower()
                if attempt + 1 < retries and any(x in msg for x in ("content modified", "cancel", "not ready", "try again")):
                    time.sleep(delay); continue
                raise
            r = r if isinstance(r, list) else [r] if r else []
            out = []
            for l in r:
                u, rg = l.get("uri") or l.get("targetUri"), l.get("range") or l.get("targetSelectionRange")
                if not u or not rg: continue
                f = Path(unuri(u))
                try: rel = str(f.relative_to(self.root))
                except ValueError: rel = str(f)
                ln = rg["start"]["line"]
                lines = f.read_text(encoding="utf-8", errors="replace").splitlines() if f.exists() else []
                text = lines[ln].strip() if 0 <= ln < len(lines) else ""
                out.append({"path": rel, "line": ln + 1, "col": rg["start"]["character"] + 1, "text": text})
            if out or attempt + 1 >= retries: return out
            time.sleep(delay)  # rust-analyzer may still be indexing; bounded wait

    def _retries(self, p, method):
        # rust-analyzer answers before its background indexing is complete.
        return 40 if self.lsp(p).key == "rust-analyzer" and method in ("textDocument/definition", "textDocument/references") else 1

    def _ident_at(self, path, line, col):
        """Identifier under/near a 1-based position, for lexical fallback."""
        try:
            lines = self.abs(path).read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError:
            return None
        if not (1 <= line <= len(lines)) or not lines[line - 1]: return None
        s = lines[line - 1]
        i = min(max(col - 1, 0), len(s) - 1)
        idc = lambda c: c.isalnum() or c == "_"
        if not idc(s[i]):
            if i + 1 < len(s) and idc(s[i + 1]): i += 1
            elif i > 0 and idc(s[i - 1]): i -= 1
            else: return None
        a = i
        while a > 0 and idc(s[a - 1]): a -= 1
        b = i
        while b + 1 < len(s) and idc(s[b + 1]): b += 1
        return s[a:b + 1]

    def _fallback_definition(self, path, line, col):
        """Symbol-table lookup used when a server (e.g. rust-analyzer without
        rust-src) cannot resolve a name inside a macro invocation."""
        name = self._ident_at(path, line, col)
        if not name: return []
        here = self.abs(path)
        seen, out = set(), []
        for f in [here, *self._files(".")]:
            f = f.resolve()
            if f in seen: continue
            seen.add(f)
            try: syms = self.lsp(f).req("textDocument/documentSymbol", {"textDocument": {"uri": uri(f)}})
            except Exception: continue
            lines = f.read_text(encoding="utf-8", errors="replace").splitlines()
            for s in self._flat(syms, f):
                if s["name"].rsplit("/", 1)[-1] != name: continue
                ln = s["line"]
                out.append({"path": s["path"], "line": ln, "col": s["col"],
                            "text": lines[ln - 1].strip() if 0 < ln <= len(lines) else ""})
                if len(out) >= 20: return out
        return out

    def _highlight_refs(self, path, line, col):
        """sourcekit-lsp (and others) may answer documentHighlight before
        references while the index is still warming up."""
        p = self.abs(path)
        try:
            r = self.lsp(p).req("textDocument/documentHighlight",
                                {"textDocument": {"uri": uri(p)}, "position": {"line": line - 1, "character": col - 1}})
        except Exception:
            return []
        r = r if isinstance(r, list) else [r] if r else []
        try: decl = {(d["line"], d["col"]) for d in self._locs("textDocument/definition", path, line, col, retries=1)}
        except Exception: decl = set()
        if not decl: decl = {(d["line"], d["col"]) for d in self._fallback_definition(path, line, col)}
        if not decl: decl = {(line, col)}
        lines = p.read_text(encoding="utf-8", errors="replace").splitlines()
        out = []
        for h in r:
            rg = h.get("range") or {}
            st = rg.get("start") or {}
            ln, ch = st.get("line", 0) + 1, st.get("character", 0) + 1
            if (ln, ch) in decl: continue
            out.append({"path": str(p.relative_to(self.root)), "line": ln, "col": ch,
                        "text": lines[ln - 1].strip() if 0 < ln <= len(lines) else ""})
        return out

    def _text_refs(self, path, line, col):
        """Last-resort lexical references when the server cannot resolve the
        symbol (macro body, missing sysroot/index). Declaration is excluded."""
        name = self._ident_at(path, line, col)
        if not name: return []
        decl = {(d["line"], d["col"]) for d in self._fallback_definition(path, line, col)}
        if not decl: decl = {(line, col)}
        pat, out = re.compile(r"\b" + re.escape(name) + r"\b"), []
        for f in self._files("."):
            try: text = f.read_text(encoding="utf-8", errors="replace")
            except OSError: continue
            lines = text.splitlines()
            for m in pat.finditer(text):
                ln = text.count("\n", 0, m.start()) + 1
                ch = m.start() - (text.rfind("\n", 0, m.start()) + 1) + 1
                if (ln, ch) in decl: continue
                out.append({"path": str(f.relative_to(self.root)), "line": ln, "col": ch,
                            "text": lines[ln - 1].strip() if 0 < ln <= len(lines) else ""})
                if len(out) >= 200: return out
        return out

    def code_definition(self, path, line, col):
        p = self.abs(path)
        try:
            out = self._locs("textDocument/definition", path, line, col, retries=self._retries(p, "textDocument/definition"))
        except Exception:
            out = []
        return out or self._fallback_definition(path, line, col)

    def code_references(self, path, line, col):
        p = self.abs(path)
        try:
            out = self._locs("textDocument/references", path, line, col, {"context": {"includeDeclaration": False}},
                             retries=self._retries(p, "textDocument/references"))
        except Exception:
            out = []
        if not out: out = self._highlight_refs(path, line, col)
        if not out: out = self._text_refs(path, line, col)
        return out

    def code_replace_body(self, path, name, body):
        p = self.abs(path)
        hits = [s for s in self._flat(self.lsp(p).req("textDocument/documentSymbol", {"textDocument": {"uri": uri(p)}}), p) if s["name"] == name or s["name"].endswith("/" + name)]
        if len(hits) != 1: raise ValueError(f"need exactly one symbol named {name}, found {len(hits)}")
        r = hits[0]["range"]; lines = p.read_text(encoding="utf-8").splitlines(keepends=True)
        a = sum(len(x) for x in lines[:r["start"]["line"]]) + r["start"]["character"]
        b = sum(len(x) for x in lines[:r["end"]["line"]]) + r["end"]["character"]
        t = "".join(lines); p.write_text(t[:a] + body + t[b:], encoding="utf-8")
        self.servers.pop(LSP[p.suffix][0]).close()  # reload cleanly next time
        return {"path": hits[0]["path"], "replaced": name, "lines": [r["start"]["line"] + 1, r["end"]["line"] + 1]}

    # ---- ops graph (existing kiwi-graph, read-only commands) ----
    def ops_graph(self, args):
        if not args or args[0] not in {"rules", "trace", "owners", "preflight", "validate"}:
            raise ValueError("ops_graph allows: rules, trace, owners, preflight, validate")
        exe = os.environ.get("KIWI_GRAPH_BIN") or shutil.which("kiwi-graph") or "/tmp/kiwi-graph-target/release/kiwi-graph"
        if not Path(exe).exists() and not shutil.which(exe): raise RuntimeError("kiwi-graph binary not built (bootstrap.sh)")
        r = subprocess.run([exe, *args], capture_output=True, text=True, timeout=120)
        return {"exit": r.returncode, "out": r.stdout[-20000:], "err": r.stderr[-2000:]}

    # ---- docs graph (existing brain-search / EmbeddingGemma) ----
    def docs_search(self, query, k=5):
        home = Path.home()
        for c in [os.environ.get("KIWI_DOCS_SEARCH"), home / ".kiwi/bin/kiwi-search", home / ".kiwi/bin/brain-search"]:
            if c and Path(c).exists():
                r = subprocess.run([str(c), query, "-k", str(k)], capture_output=True, text=True, timeout=300)
                out = "\n".join(l for l in r.stdout.splitlines() if not _line_denied(l))
                return {"engine": "brain-search (EmbeddingGemma)", "exit": r.returncode, "out": out[-20000:]}
        if not shutil.which("rg"):
            return {"engine": "none (semantic index lives on Mac mini)", "out": "rg not found; no local docs index on this box"}
        root = Path(KIWI)
        globs = ["-g", "*.md", "-g", "*.toml", "-g", "*.txt", "-g", ".env*", "-g", "*.key", "-g", "*.pem", "-g", "!.git"]
        r = subprocess.run(["rg", "-n", "-F", "-i", "-m", "3", "--hidden", "--no-ignore", *globs, "--", query, str(root)], capture_output=True, text=True)
        keep = [l for l in r.stdout.splitlines() if not denied(Path(l.split(":", 1)[0]).resolve().relative_to(root.resolve()) if l.startswith(str(root)) else l.split(":", 1)[0])]
        return {"engine": "rg (box fallback; semantic index lives on Mac mini)", "out": "\n".join(keep[: k * 3])}

    # ---- lessons (write) ----
    def record_lesson(self, what, when, correct, reporter, where="", area="", title="", commit=True, principal=""):
        import kiwi_lessons
        return self.brief.guarded("record_lesson", principal or reporter,
                                  lambda: kiwi_lessons.record_lesson(self.root, what, when, correct, reporter, where,
                                                                     area, title, commit, events=self.events))

    def record_decision(self, line, reporter, commit=True, principal=""):
        import kiwi_lessons
        return self.brief.guarded("record_decision", principal or reporter,
                                  lambda: kiwi_lessons.record_decision(self.root, line, reporter, commit))

    # ---- self-improvement loops (kiwi_selfimprove) ----
    def rule_proposals(self, limit=20):
        return self.selfimprove.proposals.list(limit=limit)

    def brief_feedback(self, bundle_id="", task="", principal="", useful=None, noise=None, missing="", note="",
                       useful_ids=None, noise_ids=None, missing_text=""):
        if useful_ids is not None:
            base = [useful] if isinstance(useful, str) else list(useful or [])
            extra = [useful_ids] if isinstance(useful_ids, str) else list(useful_ids or [])
            useful = base + extra
        if noise_ids is not None:
            base = [noise] if isinstance(noise, str) else list(noise or [])
            extra = [noise_ids] if isinstance(noise_ids, str) else list(noise_ids or [])
            noise = base + extra
        if missing_text and not missing:
            missing = missing_text
        return self.brief.guarded("brief_feedback", principal,
                                  lambda: self.selfimprove.feedback.record(
                                      principal, bundle_id=bundle_id, task=task, useful=useful, noise=noise,
                                      missing=missing, note=note))

    def brief_quality(self, days=None):
        return self.selfimprove.feedback.quality(days)

    def decide_calibration(self, days=None, principal="", reporter="self-improve"):
        return self.brief.guarded("decide_calibration", principal or reporter,
                                  lambda: self.selfimprove.calibration.report(days, reporter=reporter))

    # ---- claims and realtime events (kiwi_events) ----
    def claim_register(self, repo, task, agent_id, bot="", branch="", paths=None, hours=None, status=None, principal=""):
        return self.brief.guarded("claim_register", principal or agent_id,
                                  lambda: self.claims.register(repo, task, agent_id, bot=bot, branch=branch,
                                                               paths=paths, hours=hours, status=status))

    def claim_renew(self, claim, agent_id, hours=None, principal=""):
        return self.brief.guarded("claim_renew", principal or agent_id,
                                  lambda: self.claims.renew(claim, agent_id, hours=hours))

    def claim_release(self, claim, agent_id, principal=""):
        return self.brief.guarded("claim_release", principal or agent_id,
                                  lambda: self.claims.release(claim, agent_id))

    def claim_list(self, repo=None, agent_id=None):
        return self.claims.list(repo=repo, agent_id=agent_id)

    def subscribe(self, url, events=None, description="", principal=""):
        return self.brief.guarded("subscribe", principal,
                                  lambda: self.events.subscribe(url, events=events, description=description))

    def unsubscribe(self, id):
        return self.events.unsubscribe(id)

    def subscriptions(self):
        return self.events.subscriptions()

    def tokens_get(self):
        return self.tokens.get()

    def token_set(self, path, value, reporter, description="", principal="", create=False):
        return self.brief.guarded("token_set", principal or reporter,
                                  lambda: self.tokens.set(path, value, reporter, description=description, create=bool(create)))

    def dsh_host(self, mac_only=False):
        """Pick the dsh host and coding model from tokens/tokens.json dsh_host (same logic as kiwi_dsh_host.py)."""
        return kiwi_dsh_host.select_host(bool(mac_only), root=self.root)

    def query(self, q, path=".", subject=None):
        res = {"code": [], "docs": None, "ops": None}
        try: res["code"] = self.code_find(q, path)
        except Exception as e: res["code"] = str(e)
        res["docs"] = self.docs_search(q)
        if subject:
            try: res["ops"] = self.ops_graph(["preflight", "--repo", str(KIWI), "--subject", subject])
            except Exception as e: res["ops"] = str(e)
        return res

    def decide(self, kind, question, input, options=None, principal=""):
        import decide as d
        result = self.brief.guarded("decide", principal,
                                    lambda: _decisions_call(d.decide, kind, question, input, options))
        if isinstance(result, dict):
            try:
                import ask
                recorded = ask.record_decide(kind, question, input, options, result, principal)
                result = {**result, "ask_id": recorded["ask_id"], "prediction": recorded["prediction"],
                          "calibration": recorded}
            except Exception as error:  # a calibration write must not lose the decide answer
                result = {**result, "calibration_error": str(error)[:300]}
        return result

    def ask(self, question, options=None, task="", principal=""):
        import ask
        threshold = self.selfimprove.calibration.threshold()
        return self.brief.guarded("ask", principal,
                                  lambda: _decisions_call(ask.ask, question, options, task, principal, threshold=threshold))

    def ask_resolve(self, ask_id, actual):
        import ask
        return _decisions_call(ask.resolve, ask_id, actual)

    def ask_stats(self, days=30):
        import ask
        return _decisions_call(ask.stats, days)

    # Kiwi v2: one call at task start returns only what this task needs (dev/tools/kiwi-core).
    def _kiwi(self, *args):
        exe = os.environ.get("KIWI_BIN") or shutil.which("kiwi") or str(Path.home() / ".local/bin/kiwi")
        r = subprocess.run([exe, *args], capture_output=True, text=True, timeout=120)
        if r.returncode not in (0, 9, 10, 11): raise RuntimeError("kiwi: " + (r.stderr.strip() or str(r.returncode))[:400])
        return json.loads(r.stdout)

    def context_for(self, principal, task, actions=None, paths=None, budget=4096, task_id="", scope="*", question=None, options=None):
        self.brief.record_brief(principal, task, "context_for")
        bundle = self._context_bundle(principal, task, actions, paths, budget, task_id, scope)
        threshold = self.selfimprove.calibration.threshold()
        try:
            selected, applied = self.selfimprove.feedback.reorder(bundle.get("selected_context") or [], principal)
            if applied.get("applied"):
                bundle["selected_context"] = selected
            if applied.get("feedback"):
                bundle["brief_feedback"] = applied
        except Exception as error:  # feedback must never break the brief
            bundle["self_improve_error"] = str(error)[:200]
        try:
            repeated = self.selfimprove.feedback.missing_repeated(principal)
            if repeated:
                bundle["missing_repeated"] = repeated
        except Exception:
            pass
        if question is None:
            question = task
        pass_part = calibrated_pass(task, self.brief.config())
        if pass_part is not None:
            part, ask_result = pass_part, None
        elif os.environ.get("KIWI_CONTEXT_ANSWER", "").lower() == "off":
            part, ask_result = _answer_part(None, threshold), None
            part["needs_user"] = {"reason": "disabled"}
        else:
            key = (principal, task, question, tuple(options or []), threshold)
            now = time.time()
            cached = CONTEXT_CACHE.get(key)
            if cached and now - cached[0] < CONTEXT_CACHE_TTL:
                _, part, ask_result = cached
            else:
                CONTEXT_CACHE.pop(key, None)
                try:
                    ask_result = self.ask(question, options, task=task, principal=principal)
                    part = _answer_part(ask_result, threshold)
                except (Exception, SystemExit):
                    ask_result, part = None, _answer_part(None, threshold)
                else:
                    if isinstance(ask_result, dict):
                        CONTEXT_CACHE[key] = (now, part, ask_result)
        bundle["answer_detail"] = ask_result
        return {**part, **bundle}

    def _context_bundle(self, principal, task, actions=None, paths=None, budget=4096, task_id="", scope="*"):
        args = ["--principal", principal, "--task", task, "--budget", str(budget), "--scope", scope]
        if actions: args += ["--actions", ",".join(actions)]
        if paths: args += ["--paths", ",".join(paths)]
        if task_id: args += ["--task-id", task_id]
        if os.environ.get("KIWI_DECISIONS", "on") != "off":
            try:
                import adapter
                return adapter.brief(args)
            except Exception:
                pass  # Decisions/adapter unavailable: plain bundle with local scores
        return self._kiwi("brief", *args)

    def kiwi_rule(self, id):
        if denied(id): raise ValueError("denied")
        return self._kiwi("rule", id)

    def kiwi_status(self):
        try:
            status = self._kiwi("status")
        except Exception as error:  # audit is local; still report it when core is unavailable
            return {"status_error": str(error)[:400], "brief_audit": self.brief.audit(),
                    "self_improve": self.selfimprove.summary()}
        if isinstance(status, dict):
            status["brief_audit"] = self.brief.audit()
            status["self_improve"] = self.selfimprove.summary()
        return status

    def task_guard(self, task_id, fencing_token, principal=""):
        self.brief.record_guard(principal, task_id)
        return self._kiwi("task", "guard", "--task-id", task_id, "--fencing-token", str(fencing_token))

    def rule_resources(self):
        """kiwi://rule/{id} for every compiled node (kiwi.db holds rules/bots/skills only; that is scope, not a block)."""
        import sqlite3
        db = os.environ.get("KIWI_DB") or str(Path(os.environ.get("KIWI_HOME", str(Path.home() / ".kiwi"))) / "kiwi.db")
        if not Path(db).is_file(): return []
        c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        return [{"uri": f"kiwi://rule/{i}", "name": i, "title": t or i, "description": (s or "")[:200], "mimeType": "text/markdown"}
                for i, t, s, p in c.execute("SELECT id, title, summary, path FROM nodes ORDER BY id") if not denied(p)]

    def read_resource(self, uri):
        if not uri.startswith("kiwi://rule/"): raise ValueError("unknown resource " + uri)
        r = self.kiwi_rule(uri[len("kiwi://rule/"):])
        return {"contents": [{"uri": uri, "mimeType": "text/markdown", "text": r["body"]}]}

    def close(self):
        for s in self.servers.values(): s.close()


S = {"type": "string"}; I = {"type": "integer"}
P = {"type": "string", "description": "Optional agent ID for brief_guard; call context_for first."}
TOOLS = {
    "query": ("One query over code symbols, docs (brain-search) and optional ops subject (kiwi-graph).", {"q": S, "path": S, "subject": S}, ["q"]),
    "code_overview": ("List symbols in a file.", {"path": S}, ["path"]),
    "code_find": ("Find symbols by name under a path.", {"name": S, "path": S}, ["name"]),
    "code_definition": ("Go to definition at 1-based line/col.", {"path": S, "line": I, "col": I}, ["path", "line", "col"]),
    "code_references": ("Find references at 1-based line/col.", {"path": S, "line": I, "col": I}, ["path", "line", "col"]),
    "code_replace_body": ("Replace one symbol's full source range with new text (writes file).", {"path": S, "name": S, "body": S}, ["path", "name", "body"]),
    "ops_graph": ("Read-only kiwi-graph query: args list, first is rules|trace|owners|preflight|validate.", {"args": {"type": "array", "items": S}}, ["args"]),
    "decide": ("OpenAI Decisions API (tools/decisions): kind predicate|choice|score, options = choices or levels low..high. Not for final human decisions.", {"kind": S, "question": S, "input": S, "options": {"type": "array", "items": S}, "principal": P}, ["kind", "question", "input"]),
    "ask": ("Kiwi answers directly with evidence and confidence; risky or low-confidence questions return 오너 확인 필요.", {"question": S, "options": {"type": "array", "items": S}, "task": S, "principal": P}, ["question"]),
    "ask_resolve": ("Record the actual outcome of a Kiwi ask or decide prediction (decide results carry an ask_id) and update its evidence weights.", {"ask_id": S, "actual": S}, ["ask_id", "actual"]),
    "ask_stats": ("Read Kiwi ask statistics for the last days; does not write reports.", {"days": I}, []),
    "rule_proposals": ("List append-only rule proposals under inbox/rule-proposals/ (grouped similar lessons, cited rule; rules/ is never edited).", {"limit": I}, []),
    "brief_feedback": ("Append brief usefulness feedback (JSONL under logs/brief-feedback/): useful/noise context ids, missing free text, note.",
                       {"bundle_id": S, "task": S, "principal": S, "useful": {"type": "array", "items": S},
                        "noise": {"type": "array", "items": S}, "missing": S, "note": S,
                        "useful_ids": {"type": "array", "items": S}, "noise_ids": {"type": "array", "items": S},
                        "missing_text": S}, ["principal"]),
    "brief_quality": ("Report per-item useful-vs-noise rates and repeated missing text from logs/brief-feedback/.", {"days": I}, []),
    "decide_calibration": ("Report ask/decide accuracy per confidence bucket; when evidence suffices move the confidence threshold via token_set (token.changed), bounded by self_improve.calibration.",
                           {"days": I, "principal": P, "reporter": S}, []),
    "context_for": ("Kiwi task start: returns user_intents, mandatory_rules (never truncated), selected_context within budget, required_checks, owners, blocked_actions, omitted_refs, source_git_sha, freshness. Call once at task start instead of reading all rules.",
                    {"principal": S, "task": S, "actions": {"type": "array", "items": S}, "paths": {"type": "array", "items": S}, "budget": I, "task_id": S, "scope": S, "question": S, "options": {"type": "array", "items": S}}, ["principal", "task"]),
    "kiwi_rule": ("Fetch one Kiwi node by id (for omitted_refs; kiwi://rule/{id}).", {"id": S}, ["id"]),
    "kiwi_status": ("Kiwi status: active sha, intent epoch, writer lease, outbox, brief stats, and brief_audit (last brief per principal, acting calls without a brief in the last 24h).", {}, []),
    "task_guard": ("Check a running task against the latest user intent epoch: verdict ok|revise|cancelled|fenced.", {"task_id": S, "fencing_token": I, "principal": P}, ["task_id", "fencing_token"]),
    "docs_search": ("Semantic Kiwi doc search via existing brain-search (EmbeddingGemma); rg fallback on box.", {"query": S, "k": I}, ["query"]),
    "record_lesson": ("Record a mistake/lesson in Kiwi lessons/ directly (dedupes into occurrences, commits). Call this yourself; do not ask 관리 봇.",
                      {"what": S, "when": S, "where": S, "correct": S, "reporter": S, "area": S, "title": S, "principal": P},
                      ["what", "when", "correct", "reporter"]),
    "record_decision": ("Append one decision line to logs/decisions-inbox/ (dedupes, commits).", {"line": S, "reporter": S, "principal": P}, ["line", "reporter"]),
    "claim_register": ("Register a bots/active claim (writes + commits the TOML, emits claim.registered). Omit branch to register by task description.",
                       {"repo": S, "task": S, "agent_id": S, "bot": S, "branch": S, "paths": {"type": "array", "items": S}, "hours": I, "status": S, "principal": P},
                       ["repo", "task", "agent_id"]),
    "claim_renew": ("Extend a claim you hold (commits, emits claim.renewed).", {"claim": S, "agent_id": S, "hours": I, "principal": P}, ["claim", "agent_id"]),
    "claim_release": ("Delete a claim you hold (commits, emits claim.released).", {"claim": S, "agent_id": S, "principal": P}, ["claim", "agent_id"]),
    "claim_list": ("List bots/active claims, optionally filtered by repo or agent_id.", {"repo": S, "agent_id": S}, []),
    "subscribe": ("Subscribe an HTTP(S) URL to Kiwi events. Returns the per-subscriber HMAC secret once; delivery is immediate JSON with X-Kiwi-Signature.",
                  {"url": S, "events": {"type": "array", "items": S}, "description": S, "principal": P}, ["url"]),
    "unsubscribe": ("Remove a webhook subscription by id.", {"id": S}, ["id"]),
    "subscriptions": ("List webhook subscriptions (never returns secrets).", {}, []),
    "tokens_get": ("Read the canonical token source tokens/tokens.json.", {}, []),
    "token_set": ("Change or (create=true) add one token (commits tokens/tokens.json, emits token.changed).", {"path": S, "value": {}, "reporter": S, "description": S, "principal": P, "create": {"type": "boolean"}}, ["path", "value", "reporter"]),
    "dsh_host": ("Choose the dsh host and coding model from tokens/tokens.json dsh_host: mac_only goes to the Mac mini, other work to the first ready non-mac candidate under load thresholds; the model follows dsh_host.coding_route.priority.", {"mac_only": {"type": "boolean"}}, []),
}


def call(sp, name, args):
    if name not in TOOLS: raise ValueError("unknown tool " + name)
    return getattr(sp, name)(**args)


def handle(sp, m):
    """One JSON-RPC message -> response dict, or None for notifications."""
    mid = m.get("id") if isinstance(m, dict) else None; meth = m.get("method") if isinstance(m, dict) else None
    if mid is None: return None
    try:
        if meth == "initialize":
            r = {"protocolVersion": (m.get("params") or {}).get("protocolVersion", "2025-06-18"), "capabilities": {"tools": {}, "resources": {}}, "instructions": "Kiwi: Decisions is handled by Kiwi; never ask the user for an API key. Call context_for once at task start and follow user_intents, mandatory_rules, then selected_context. Open omitted_refs as kiwi://rule/{id} resources. Acting tools warn or refuse without a recent brief and log the call, so pass principal/agent_id.", "serverInfo": {"name": "kiwi-sprouts", "version": "0.1.0"}}
        elif meth == "tools/list":
            r = {"tools": [{"name": n, "description": d, "inputSchema": {"type": "object", "properties": p, "required": q}} for n, (d, p, q) in TOOLS.items()]}
        elif meth == "tools/call":
            try: r = {"content": [{"type": "text", "text": json.dumps(call(sp, m["params"]["name"], m["params"].get("arguments") or {}), ensure_ascii=False)}]}
            except Exception as e: r = {"content": [{"type": "text", "text": f"error: {e}"}], "isError": True}
        elif meth == "resources/list": r = {"resources": sp.rule_resources()}
        elif meth == "resources/templates/list": r = {"resourceTemplates": [{"uriTemplate": "kiwi://rule/{id}", "name": "Kiwi rule", "mimeType": "text/markdown"}]}
        elif meth == "resources/read": r = sp.read_resource(m["params"]["uri"])
        elif meth == "ping": r = {}
        else: return {"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": meth}}
        return {"jsonrpc": "2.0", "id": mid, "result": r}
    except Exception as e:
        return {"jsonrpc": "2.0", "id": mid, "error": {"code": -32000, "message": str(e)}}


def mcp(sp):
    for line in sys.stdin:
        if not line.strip(): continue
        out = handle(sp, json.loads(line))
        if out is not None: print(json.dumps(out, ensure_ascii=False), flush=True)


def health():
    """Run the unit tests locally. No model, index, or network is used."""
    tests = HERE / "tests" / "test_sprouts.py"
    if not tests.is_file():
        print(f"health: tests not found: {tests}", file=sys.stderr)
        return 2
    cmd = [sys.executable, "-m", "unittest", "-v", str(tests)]
    print("health: " + " ".join(cmd), file=sys.stderr, flush=True)
    return subprocess.call(cmd, cwd=str(HERE), env=dict(os.environ))


def main(argv):
    if argv[:1] == ["--health"]: return health()
    root = None
    if argv[:1] == ["--root"]: root, argv = argv[1], argv[2:]
    sp = Sprouts(root)
    try:
        if not argv or argv[0] == "mcp": mcp(sp)
        else: print(json.dumps(call(sp, argv[0], json.loads(argv[1]) if len(argv) > 1 else {}), ensure_ascii=False, indent=1))
    finally: sp.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
