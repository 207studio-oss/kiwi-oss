#!/usr/bin/env python3
"""Realtime Kiwi state: claim board writes, HMAC webhooks, and design tokens.

The claim board in ``bots/active/`` is still one TOML file per task, but it is
now written and committed by tools instead of by hand.  Every change emits a
``claim.*`` event to subscribed webhooks.  Token changes in
``tokens/tokens.json`` go through the same event bus as ``token.changed``.

Storage:
  ~/.kiwi/state/kiwi-live-subscriptions.json  (secrets, chmod 600)
  ~/.kiwi/state/kiwi-live-events.jsonl        (delivery audit, no secrets)
  <repo>/bots/active/*.toml                   (claim board, committed)
  <repo>/tokens/tokens.json                   (canonical tokens, committed)
"""
import datetime
import hashlib
import hmac
import json
import os
import queue
import re
import secrets
import subprocess
import threading
import time
import tomllib
import unicodedata
import uuid
from pathlib import Path
from urllib.parse import urlparse
from urllib.request import Request, urlopen, build_opener, ProxyHandler


def _webhook_opener():
    """KIWI_WEBHOOK_PROXY (e.g. http://127.0.0.1:1055, tailscaled --outbound-http-proxy-listen) reaches tailnet
    subscribers from a userspace-networking box; unset means direct."""
    proxy = os.environ.get("KIWI_WEBHOOK_PROXY", "").strip()
    return build_opener(ProxyHandler({"http": proxy, "https": proxy} if proxy else {}))

SEOUL = datetime.timezone(datetime.timedelta(hours=9))
STATE_DIR = Path(os.environ.get("KIWI_LIVE_STATE_DIR") or "~/.kiwi/state").expanduser()
# Same guard as kiwi_live/kiwi_lessons: never persist credentials.
SECRET_LINE = re.compile(
    r"((?<![A-Za-z])sk-[A-Za-z0-9_-]{16,}|gh[pousr]_[A-Za-z0-9]{20,}|xox[abpr]-|AKIA[0-9A-Z]{16}|"
    r"-----BEGIN [A-Z ]*PRIVATE KEY|(?i:(password|passwd|secret|token|api[_-]?key|비밀번호)\s*[:=]\s*\S{6,}))"
)
SAFE_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,80}$")
REPO_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*/[A-Za-z0-9][A-Za-z0-9._-]*$")
EVENT_NAME = re.compile(r"^[a-z][a-z0-9_.-]{1,80}$")
TOKEN_PATH = re.compile(r"^[a-z][a-z0-9_]*(\.[a-z0-9_]+)+$")
CLAIM = re.compile(r"^bots/active/[A-Za-z0-9][A-Za-z0-9._-]*\.toml$")


def now():
    return datetime.datetime.now(SEOUL).isoformat(timespec="seconds")


def _slug(text, limit=60):
    text = unicodedata.normalize("NFC", str(text).strip().casefold())
    slug = re.sub(r"-+", "-", "".join(ch if ch.isalnum() else "-" for ch in text)).strip("-")
    return (slug or "task")[:limit].strip("-")


def _git(root, *args):
    try:
        return subprocess.run(["git", "-C", str(root), *args], capture_output=True, text=True,
                              encoding="utf-8", errors="replace")
    except OSError as error:
        return subprocess.CompletedProcess(args, 127, "", str(error))


def _commit(root, rel, message):
    """Commit exactly one board/token path; return its SHA (or None if unchanged)."""
    added = _git(root, "add", "--", rel)
    if added.returncode != 0:
        raise RuntimeError("git add failed: " + (added.stderr or added.stdout).strip())
    if _git(root, "diff", "--cached", "--quiet", "--", rel).returncode == 0:
        return None
    result = _git(root, "-c", "user.name=kiwi", "-c", "user.email=kiwi@example.com",
                  "commit", "-q", "-m", message, "--", rel)
    if result.returncode != 0:
        raise RuntimeError("git commit failed: " + (result.stderr or result.stdout).strip())
    return _git(root, "rev-parse", "HEAD").stdout.strip()


def _read_exact(path):
    """Read without newline translation (Path.read_text gained newline= only in Python 3.13)."""
    with open(path, encoding="utf-8", newline="") as handle:
        return handle.read()


def _restore(root, rel, old):
    _git(root, "reset", "-q", "--", rel)
    path = root / rel
    if old is None:
        path.unlink(missing_ok=True)
    else:
        path.write_text(old, encoding="utf-8", newline="")


class EventBus:
    """Per-subscriber HMAC webhooks with a single immediate-delivery worker."""

    FILE = "kiwi-live-subscriptions.json"
    LOG = "kiwi-live-events.jsonl"

    def __init__(self, state_dir=None):
        self.dir = Path(state_dir or STATE_DIR).expanduser()
        self.path = self.dir / self.FILE
        self.log = self.dir / self.LOG
        self._lock = threading.RLock()
        self._queue = queue.Queue()
        self._stop = threading.Event()
        self._worker = None
        self._data = self._load()

    # ---- subscriptions -------------------------------------------------
    def _load(self):
        try:
            data = json.loads(self.path.read_text(encoding="utf-8"))
            return data if data.get("version") == 1 and isinstance(data.get("subscribers"), list) else {"version": 1, "subscribers": []}
        except (OSError, ValueError):
            return {"version": 1, "subscribers": []}

    def _save(self):
        self.dir.mkdir(parents=True, exist_ok=True)
        tmp = self.path.with_suffix(".tmp")
        tmp.write_text(json.dumps(self._data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        os.chmod(tmp, 0o600)
        os.replace(tmp, self.path)

    @staticmethod
    def _url(url):
        if not isinstance(url, str) or not url.strip() or len(url) > 2000:
            raise ValueError("url must be a non-empty http(s) URL")
        if any(ord(ch) < 0x20 or ord(ch) == 0x7f for ch in url):
            raise ValueError("url contains control characters")
        parsed = urlparse(url)
        if parsed.scheme not in ("http", "https") or not parsed.netloc or parsed.username or parsed.password:
            raise ValueError("url must be http(s) without userinfo")
        return url

    @staticmethod
    def _events(events):
        if events is None:
            return ["*"]
        if isinstance(events, str):
            events = [events]
        if not isinstance(events, list) or not events or len(events) > 50:
            raise ValueError("events must be a non-empty list of at most 50 names")
        out = []
        for name in events:
            if not isinstance(name, str) or not (name == "*" or EVENT_NAME.match(name)):
                raise ValueError("bad event name: " + str(name))
            if name not in out:
                out.append(name)
        return out

    def subscribe(self, url, events=None, description=""):
        url = self._url(url.strip())
        events = self._events(events)
        description = str(description or "").strip()[:300]
        if SECRET_LINE.search(description):
            raise ValueError("refusing to store a secret in a subscription description")
        with self._lock:
            sub = {"id": "sub-" + uuid.uuid4().hex[:12], "url": url, "events": events,
                   "description": description, "secret": secrets.token_urlsafe(32),
                   "created": now(), "active": True, "deliveries": 0, "failures": 0}
            self._data["subscribers"].append(sub)
            self._save()
            return dict(sub)

    def unsubscribe(self, id):
        if not isinstance(id, str) or not id.strip():
            raise ValueError("id is required")
        with self._lock:
            before = len(self._data["subscribers"])
            self._data["subscribers"] = [s for s in self._data["subscribers"] if s.get("id") != id.strip()]
            if len(self._data["subscribers"]) == before:
                raise ValueError("unknown subscription: " + id)
            self._save()
            return {"unsubscribed": id, "remaining": len(self._data["subscribers"])}

    def subscriptions(self):
        with self._lock:
            return [{"id": s["id"], "url": s["url"], "events": list(s["events"]),
                     "description": s.get("description", ""), "created": s["created"],
                     "active": s.get("active", True), "deliveries": s.get("deliveries", 0),
                     "failures": s.get("failures", 0)} for s in self._data["subscribers"]]

    # ---- events --------------------------------------------------------
    def _record(self, event):
        try:
            self.dir.mkdir(parents=True, exist_ok=True)
            with self._lock, self.log.open("a", encoding="utf-8") as handle:
                handle.write(json.dumps(event, ensure_ascii=False, separators=(",", ":")) + "\n")
        except OSError:
            pass  # audit failures must not silently break a claim write

    def emit(self, event, data):
        if not isinstance(event, str) or not EVENT_NAME.match(event):
            raise ValueError("bad event name: " + str(event))
        envelope = {"schema": 1, "id": "evt-" + uuid.uuid4().hex,
                    "event": event, "created": now(), "source": "kiwi-live", "data": data}
        with self._lock:
            targets = [s for s in self._data["subscribers"]
                       if s.get("active", True) and ("*" in s.get("events", []) or event in s.get("events", []))]
        self._record(envelope)
        for sub in targets:
            self._queue.put((dict(sub), envelope))
        if targets:
            self._start()
        return {"id": envelope["id"], "event": event, "created": envelope["created"],
                "subscribers": len(targets)}

    def _start(self):
        with self._lock:
            if self._worker and self._worker.is_alive():
                return
            self._worker = threading.Thread(target=self._run, name="kiwi-webhooks", daemon=True)
            self._worker.start()

    def _run(self):
        while not self._stop.is_set():
            try:
                sub, event = self._queue.get(timeout=0.5)
            except queue.Empty:
                continue
            try:
                self._deliver(sub, event)
            finally:
                self._queue.task_done()

    def _deliver(self, sub, event):
        body = json.dumps(event, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        ok = False
        error = ""
        for delay in (0, 1, 3):
            if delay:
                time.sleep(delay)
            ts = str(int(time.time()))
            signed = ts.encode() + b"." + body
            signature = "v1=" + hmac.new(sub["secret"].encode(), signed, hashlib.sha256).hexdigest()
            request = Request(sub["url"], body, {
                "Content-Type": "application/json",
                "User-Agent": "kiwi-live-webhook/1",
                "X-Kiwi-Event": event["event"],
                "X-Kiwi-Delivery": event["id"],
                "X-Kiwi-Timestamp": ts,
                "X-Kiwi-Signature": signature,
            })
            try:
                with _webhook_opener().open(request, timeout=5) as response:
                    response.read(256)
                    ok = 200 <= response.status < 300
            except Exception as exc:  # network errors, non-2xx, subscriber down
                error = str(exc)[:300]
            if ok:
                break
        with self._lock:
            for item in self._data["subscribers"]:
                if item.get("id") == sub["id"]:
                    item["deliveries"] = item.get("deliveries", 0) + (1 if ok else 0)
                    item["failures"] = item.get("failures", 0) + (0 if ok else 1)
                    if error:
                        item["last_error"] = error
                    self._save()
                    break

    def wait_idle(self, timeout=5):
        """Test/CLI helper: wait until queued deliveries have been attempted."""
        done = threading.Event()

        def wait():
            self._queue.join()
            done.set()

        threading.Thread(target=wait, daemon=True).start()
        return done.wait(timeout)

    def close(self):
        self._stop.set()
        if self._worker and self._worker.is_alive():
            self._worker.join(timeout=2)

    @staticmethod
    def verify(secret, timestamp, body, signature):
        """Verify X-Kiwi-Signature over ``timestamp + "." + raw_body``."""
        expected = "v1=" + hmac.new(str(secret).encode(), str(timestamp).encode() + b"." + body,
                                    hashlib.sha256).hexdigest()
        return hmac.compare_digest(expected, str(signature))


class Claims:
    """Create, renew, and release bots/active/*.toml claims and emit events."""

    def __init__(self, root, bus=None):
        self.root = Path(root).resolve()
        self.board = self.root / "bots" / "active"
        self.bus = bus or EventBus()
        self._lock = threading.RLock()

    def _registry(self):
        try:
            data = tomllib.loads((self.root / "bots" / "registry.toml").read_text(encoding="utf-8"))
        except (OSError, ValueError) as error:
            raise ValueError("bots/registry.toml is missing or unparsable: " + str(error))
        known = {}
        for bot in data.get("bot", []):
            if isinstance(bot.get("id"), str):
                known[bot["id"]] = str(bot.get("name") or "")
        if not known:
            raise ValueError("bots/registry.toml has no [[bot]] entries")
        return known

    def _tokens(self):
        try:
            return json.loads((self.root / "tokens" / "tokens.json").read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return {}

    def _hours(self, hours):
        default = self._tokens().get("claim", {}).get("default_hours", 4)
        value = default if hours is None else hours
        try:
            value = float(value)
        except (TypeError, ValueError):
            raise ValueError("hours must be a number")
        if not 0.25 <= value <= 24 * 14:
            raise ValueError("hours must be between 0.25 and 336")
        return value

    def _status(self, status):
        states = self._tokens().get("claim", {}).get("states", ["active", "expired"])
        status = str(status or "active").strip()
        if status not in states:
            raise ValueError("status must be one of: " + ", ".join(states))
        return status

    def _repo(self, repo):
        repo = str(repo or "").strip()
        if not REPO_ID.match(repo):
            raise ValueError("repo must look like owner/name")
        return repo

    @staticmethod
    def _branch(branch):
        branch = str(branch or "").strip()
        if not branch:
            return ""
        if len(branch) > 200 or "\n" in branch or "\r" in branch or ".." in branch.split("/") or branch.startswith("/"):
            raise ValueError("bad branch")
        return branch

    @staticmethod
    def _paths(paths):
        if paths is None:
            return []
        if isinstance(paths, str):
            paths = [paths]
        if not isinstance(paths, list) or len(paths) > 50:
            raise ValueError("paths must be a list of at most 50 paths")
        out = []
        for path in paths:
            path = str(path or "").strip().replace("\\", "/").removeprefix("./")
            if not path or path.startswith("/") or ".." in path.split("/") or "\n" in path:
                raise ValueError("bad path: " + str(path))
            if path not in out:
                out.append(path)
        return out

    def _rel(self, repo, branch, task):
        owner, name = repo.split("/", 1)
        key = branch.replace("/", "__") if branch else "task__" + _slug(task)
        key = re.sub(r"[^0-9A-Za-z._\uac00-\ud7a3-]+", "-", key).strip("-.") or ("task__" + _slug(task))
        return f"bots/active/{owner}--{name}--{key}.toml"

    def _resolve(self, root, claim):
        claim = str(claim or "").strip().replace("\\", "/")
        if not claim:
            raise ValueError("claim path is required")
        if claim.startswith("bots/active/"):
            rel = claim
        elif claim.endswith(".toml") and "/" not in claim:
            rel = "bots/active/" + claim
        else:
            raise ValueError("claim must be bots/active/<file>.toml or a file name")
        if not CLAIM.match(rel) or ".." in rel.split("/"):
            raise ValueError("bad claim path")
        return self.root / rel, rel

    def _read(self, path, rel):
        try:
            return tomllib.loads(path.read_text(encoding="utf-8"))
        except (OSError, ValueError) as error:
            raise ValueError(f"cannot read {rel}: {error}")

    @staticmethod
    def _expires(value):
        try:
            return datetime.datetime.fromisoformat(str(value))
        except (TypeError, ValueError):
            return None

    def _current(self, claim):
        expires = self._expires(claim.get("expires"))
        return claim.get("status", "active") != "expired" and expires is not None and expires > datetime.datetime.now(SEOUL)

    @staticmethod
    def _path_overlap(a, b):
        if not a or not b:
            return True  # empty paths mean the whole repository
        for left in a:
            for right in b:
                if left == right or left.startswith(right.rstrip("/") + "/") or right.startswith(left.rstrip("/") + "/"):
                    return True
        return False

    def _conflict(self, repo, branch, paths, agent_id):
        if not self.board.is_dir():
            return None
        for path in sorted(self.board.glob("*.toml")):
            claim = self._read(path, path.name)
            if claim.get("repo") != repo or claim.get("agent_id") == agent_id or not self._current(claim):
                continue
            if claim.get("branch_unknown") or not branch:
                continue
            if claim.get("branch") == branch or self._path_overlap(paths, claim.get("paths") or []):
                return {"path": "bots/active/" + path.name, "agent_id": claim.get("agent_id"),
                        "branch": claim.get("branch", ""), "task": claim.get("task", "")}
        return None

    def _render(self, claim):
        def q(value):
            return json.dumps(value, ensure_ascii=False)

        lines = [f"repo = {q(claim['repo'])}"]
        if claim.get("branch"):
            lines.append(f"branch = {q(claim['branch'])}")
        else:
            lines.append('branch = ""')
            lines.append("branch_unknown = true")
            lines.append("paths_unknown = true")
        if claim.get("paths"):
            lines.append("paths = [" + ", ".join(q(p) for p in claim["paths"]) + "]")
        lines += [f"bot = {q(claim.get('bot', ''))}", f"agent_id = {q(claim['agent_id'])}",
                  f"task = {q(claim['task'])}", f"started = {q(claim['started'])}",
                  f"expires = {q(claim['expires'])}"]
        if claim.get("updated"):
            lines.append(f"updated = {q(claim['updated'])}")
        if claim.get("status") and claim["status"] != "active":
            lines.append(f"status = {q(claim['status'])}")
        return "\n".join(lines) + "\n"

    def register(self, repo, task, agent_id, bot="", branch="", paths=None, hours=None, status=None):
        with self._lock:
            repo = self._repo(repo)
            task = str(task or "").strip()
            if not task or "\n" in task or "\r" in task:
                raise ValueError("task must be one non-empty line")
            if SECRET_LINE.search(task):
                raise ValueError("refusing to record a secret in a claim")
            known = self._registry()
            agent_id = str(agent_id or "").strip()
            if agent_id not in known:
                raise ValueError("agent_id is not a registered bot: " + agent_id)
            bot = str(bot or known.get(agent_id) or "").strip()
            branch = self._branch(branch)
            paths = self._paths(paths)
            status = self._status(status)
            hours = self._hours(hours)
            rel = self._rel(repo, branch, task)
            path = self.root / rel
            self.board.mkdir(parents=True, exist_ok=True)
            old = _read_exact(path) if path.is_file() else None
            if old is not None:
                existing = self._read(path, rel)
                if existing.get("agent_id") != agent_id and self._current(existing):
                    raise ValueError("claim already held by " + str(existing.get("agent_id")) + " until " + str(existing.get("expires")))
                if existing.get("agent_id") == agent_id and existing.get("task") == task and self._current(existing) and existing.get("status", "active") == status:
                    return {"action": "unchanged", "path": rel, "commit": None, "claim": existing, "event": None}
            conflict = self._conflict(repo, branch, paths, agent_id)
            if conflict:
                raise ValueError("conflict with " + conflict["path"] + " (" + str(conflict.get("agent_id")) + "): " + str(conflict.get("task")))
            started = datetime.datetime.now(SEOUL)
            claim = {"repo": repo, "branch": branch, "paths": paths, "bot": bot, "agent_id": agent_id,
                     "task": task, "started": started.isoformat(timespec="seconds"),
                     "expires": (started + datetime.timedelta(hours=hours)).isoformat(timespec="seconds"),
                     "status": status}
            path.write_text(self._render(claim), encoding="utf-8", newline="")
            try:
                commit = _commit(self.root, rel, f"claim: {task} ({bot})")
            except Exception:
                _restore(self.root, rel, old)
                raise
            event = self.bus.emit("claim.registered", {"path": rel, "commit": commit, "claim": claim})
            return {"action": "registered", "path": rel, "commit": commit, "claim": claim, "event": event}

    def renew(self, claim, agent_id, hours=None):
        with self._lock:
            known = self._registry()
            agent_id = str(agent_id or "").strip()
            if agent_id not in known:
                raise ValueError("agent_id is not a registered bot: " + agent_id)
            path, rel = self._resolve(self.root, claim)
            data = self._read(path, rel)
            if data.get("agent_id") != agent_id:
                raise ValueError("only the holder can renew this claim: " + str(data.get("agent_id")))
            hours = self._hours(hours)
            old_text = _read_exact(path)
            old_exp = self._expires(data.get("expires"))
            previous_expires = data.get("expires")
            new_exp = datetime.datetime.now(SEOUL) + datetime.timedelta(hours=hours)
            if old_exp and old_exp > new_exp:
                new_exp = old_exp
            data["expires"] = new_exp.isoformat(timespec="seconds")
            data["updated"] = now()
            data.pop("status", None)
            path.write_text(self._render(data), encoding="utf-8", newline="")
            try:
                commit = _commit(self.root, rel, f"claim: renew {data.get('task', '')} ({data.get('bot', '')})")
            except Exception:
                _restore(self.root, rel, old_text)
                raise
            event = self.bus.emit("claim.renewed", {"path": rel, "commit": commit, "claim": data,
                                                    "previous_expires": previous_expires})
            return {"action": "renewed", "path": rel, "commit": commit, "claim": data, "event": event}

    def release(self, claim, agent_id):
        with self._lock:
            known = self._registry()
            agent_id = str(agent_id or "").strip()
            if agent_id not in known:
                raise ValueError("agent_id is not a registered bot: " + agent_id)
            path, rel = self._resolve(self.root, claim)
            if not path.is_file():
                raise ValueError("claim not found: " + rel)
            data = self._read(path, rel)
            if data.get("agent_id") != agent_id:
                raise ValueError("only the holder can release this claim: " + str(data.get("agent_id")))
            old_text = _read_exact(path)
            path.unlink()
            try:
                commit = _commit(self.root, rel, f"claim: release {data.get('task', '')} ({data.get('bot', '')})")
            except Exception:
                path.write_text(old_text, encoding="utf-8", newline="")
                _restore(self.root, rel, old_text)
                raise
            event = self.bus.emit("claim.released", {"path": rel, "commit": commit, "claim": data})
            return {"action": "released", "path": rel, "commit": commit, "claim": data, "event": event}

    def list(self, repo=None, agent_id=None):
        out = []
        if not self.board.is_dir():
            return out
        for path in sorted(self.board.glob("*.toml")):
            try:
                data = tomllib.loads(path.read_text(encoding="utf-8"))
            except (OSError, ValueError):
                continue
            if repo and data.get("repo") != repo:
                continue
            if agent_id and data.get("agent_id") != agent_id:
                continue
            data["path"] = "bots/active/" + path.name
            data["active"] = self._current(data)
            out.append(data)
        return out


class Tokens:
    """Canonical design/state tokens; every write emits token.changed."""

    REL = "tokens/tokens.json"

    def __init__(self, root, bus=None):
        self.root = Path(root).resolve()
        self.path = self.root / self.REL
        self.bus = bus or EventBus()
        self._lock = threading.RLock()

    def get(self):
        try:
            return json.loads(self.path.read_text(encoding="utf-8"))
        except (OSError, ValueError) as error:
            raise ValueError("tokens/tokens.json is missing or unparsable: " + str(error))

    def set(self, path, value, reporter, description="", create=False):
        with self._lock:
            path = str(path or "").strip()
            if not TOKEN_PATH.match(path):
                raise ValueError("token path must be dotted lower_snake_case, e.g. motion.button_spring.web.duration_ms")
            reporter = str(reporter or "").strip()
            if not SAFE_ID.match(reporter):
                raise ValueError("reporter must be an agent id or account name")
            if isinstance(value, str) and SECRET_LINE.search(value):
                raise ValueError("refusing to store a secret in tokens")
            description = str(description or "").strip()[:300]
            if SECRET_LINE.search(description):
                raise ValueError("refusing to store a secret in a description")
            data = self.get()
            keys = path.split(".")
            parent = data
            for key in keys[:-1]:
                if create and isinstance(parent, dict) and key not in parent:
                    parent[key] = {}
                if not isinstance(parent, dict) or key not in parent:
                    raise ValueError("unknown token path: " + path + " (pass create=true to add it)")
                parent = parent[key]
            leaf = keys[-1]
            if not isinstance(parent, dict):
                raise ValueError("token path parent is not an object: " + path)
            if leaf not in parent and not create:
                raise ValueError("unknown token path: " + path + " (pass create=true to add it)")
            old = parent.get(leaf)
            if old is None and leaf not in parent:
                if isinstance(value, str) and not value or value is None:
                    raise ValueError("new token needs a non-empty value")
            elif isinstance(old, bool) or isinstance(value, bool):
                if not isinstance(old, bool) or not isinstance(value, bool):
                    raise ValueError("token type mismatch: expected bool")
            elif isinstance(old, (int, float)):
                if not isinstance(value, (int, float)) or isinstance(value, bool):
                    raise ValueError("token type mismatch: expected number")
            elif isinstance(old, str):
                if not isinstance(value, str):
                    raise ValueError("token type mismatch: expected string")
            elif isinstance(old, list):
                if not isinstance(value, list):
                    raise ValueError("token type mismatch: expected list")
            elif isinstance(old, dict):
                if not isinstance(value, dict):
                    raise ValueError("token type mismatch: expected object")
            if value == old:
                return {"action": "unchanged", "path": path, "old": old, "new": value, "commit": None, "event": None}
            parent[leaf] = value
            old_text = _read_exact(self.path)
            self.path.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8", newline="")
            try:
                commit = _commit(self.root, self.REL, f"tokens: {path} = {json.dumps(value, ensure_ascii=False)} ({reporter})")
            except Exception:
                self.path.write_text(old_text, encoding="utf-8", newline="")
                _restore(self.root, self.REL, old_text)
                raise
            event = self.bus.emit("token.changed", {"file": self.REL, "path": path, "old": old,
                                                    "new": value, "commit": commit,
                                                    "description": str(description or "")[:300]})
            return {"action": "changed", "path": path, "old": old, "new": value,
                    "commit": commit, "event": event}
