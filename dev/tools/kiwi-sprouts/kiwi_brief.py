#!/usr/bin/env python3
"""Brief guard: audit task-start briefs and warn or block acting calls without one.

The ledger is JSONL at ``KIWI_BRIEF_LEDGER`` or
``$KIWI_HOME/state/brief-ledger.jsonl`` (default ``~/.kiwi/state``).  Window,
mode, and routing keywords come from ``tokens/tokens.json`` (``brief_guard``),
so behaviour changes through ``token_set`` instead of a code edit.
"""
import datetime
import json
import os
import re
import threading
import time
from pathlib import Path

from kiwi_events import SEOUL

# Tools that act on Kiwi or the outside world; each must record who called it.
GUARDED_TOOLS = ("decide", "ask", "claim_register", "claim_renew", "claim_release",
                 "record_lesson", "record_decision", "token_set", "subscribe",
                 "brief_feedback", "decide_calibration")
UNIDENTIFIED = "(unidentified)"
DEFAULT_WINDOW_MIN = 60.0
DEFAULT_MODE = "warn"
DEFAULT_ROUTING_ANSWER = "진행: 저위험 전달·라우팅 작업"
LOCK = threading.Lock()


def _iso(epoch):
    return datetime.datetime.fromtimestamp(epoch, SEOUL).isoformat(timespec="seconds")


def _epoch(record):
    value = record.get("epoch")
    if isinstance(value, (int, float)):
        return float(value)
    try:
        return datetime.datetime.fromisoformat(str(record.get("ts"))).timestamp()
    except (TypeError, ValueError):
        return None


def _words(value):
    if isinstance(value, str):
        value = [value]
    if not isinstance(value, list):
        return []
    return [text for text in (str(item).strip() for item in value) if text]


def mentions(text, words, prefix=False):
    """Match keywords against task text.

    Korean keywords match anywhere (agglutinative endings).  English routing
    keywords match whole words so ``route`` does not fire on ``router``;
    risky keywords match word starts so ``delete`` also catches ``deleting``.
    """
    value = str(text or "").casefold()
    for word in words:
        key = word.casefold().strip()
        if not key:
            continue
        if re.fullmatch(r"[a-z0-9_]+", key):
            tail = "" if prefix else r"(?![a-z0-9])"
            if re.search(r"(?<![a-z0-9])" + re.escape(key) + tail, value):
                return True
        elif key in value:
            return True
    return False


def calibrated_pass(task, config):
    """Direct pass for low-risk routing/relay work; None leaves the ask path alone."""
    if not mentions(task, config["routing_keywords"]):
        return None
    if mentions(task, config["risky_keywords"], prefix=True):
        return None
    return {"status": "pass",
            "answer": config["routing_answer"] or DEFAULT_ROUTING_ANSWER,
            "confidence": max(0.8, config["routing_confidence"]),
            "evidence": [], "needs_user": None, "ask_id": None}


class BriefRequired(ValueError):
    """Block-mode refusal: the caller must run context_for first."""


class BriefGuard:
    def __init__(self, root, bus=None, path=None):
        self.root = Path(root)
        self.bus = bus
        ledger = (path or os.environ.get("KIWI_BRIEF_LEDGER")
                  or str(Path(os.environ.get("KIWI_HOME") or "~/.kiwi").expanduser()
                          / "state" / "brief-ledger.jsonl"))
        self.path = Path(os.path.expanduser(str(ledger)))

    def config(self):
        try:
            data = json.loads((self.root / "tokens" / "tokens.json").read_text(encoding="utf-8"))
        except (OSError, ValueError):
            data = {}
        cfg = data.get("brief_guard") if isinstance(data, dict) else None
        cfg = cfg if isinstance(cfg, dict) else {}
        try:
            window = float(cfg.get("window_min", DEFAULT_WINDOW_MIN))
        except (TypeError, ValueError):
            window = DEFAULT_WINDOW_MIN
        mode = str(cfg.get("mode") or DEFAULT_MODE).strip().lower()
        try:
            confidence = float(cfg.get("routing_confidence", 0.85))
        except (TypeError, ValueError):
            confidence = 0.85
        return {"window_min": max(0.0, window),
                "mode": mode if mode in ("warn", "block") else DEFAULT_MODE,
                "routing_keywords": _words(cfg.get("routing_keywords")),
                "risky_keywords": _words(cfg.get("risky_keywords")),
                "routing_answer": str(cfg.get("routing_answer") or "").strip(),
                "routing_confidence": min(1.0, max(0.0, confidence))}

    def _records(self):
        records = []
        try:
            with self.path.open(encoding="utf-8", errors="replace") as handle:
                for line in handle:
                    try:
                        record = json.loads(line)
                    except ValueError:
                        continue
                    if isinstance(record, dict):
                        records.append(record)
        except OSError:
            pass
        return records

    def _append(self, record):
        line = json.dumps(record, ensure_ascii=False, separators=(",", ":")) + "\n"
        try:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            with LOCK:
                descriptor = os.open(self.path, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
                with os.fdopen(descriptor, "a", encoding="utf-8") as handle:
                    handle.write(line)
        except OSError:
            pass  # an unwritable audit log must not break the tool call

    def _last_brief(self, principal, records=None):
        latest = None
        for record in records if records is not None else self._records():
            if record.get("event") != "brief" or record.get("principal") != principal:
                continue
            epoch = _epoch(record)
            if epoch is not None and (latest is None or epoch > latest):
                latest = epoch
        return latest

    def record_brief(self, principal, task, tool="context_for"):
        now = time.time()
        self._append({"ts": _iso(now), "epoch": round(now, 3),
                      "principal": str(principal or "").strip(), "event": "brief",
                      "tool": tool, "task": str(task or "")})

    def record_guard(self, principal, task_id):
        now = time.time()
        self._append({"ts": _iso(now), "epoch": round(now, 3),
                      "principal": str(principal or "").strip(), "event": "guard",
                      "tool": "task_guard", "task": str(task_id or "")})

    @staticmethod
    def _message(principal, status, cfg):
        window = f"{cfg['window_min']:g}"
        if status == "unidentified":
            return ("brief_guard: caller is unidentified; pass principal/agent_id and call "
                    "context_for (kiwi-live) first, then retry")
        return (f"brief_guard: no brief for {principal} in the last {window} min; "
                f"call context_for (kiwi-live) first, then retry")

    def _warning(self, principal, status, cfg, last):
        return {"status": status, "mode": cfg["mode"], "window_min": cfg["window_min"],
                "message": self._message(principal, status, cfg),
                "last_brief": _iso(last) if last is not None else None}

    def _emit_missing(self, principal, tool, status, cfg):
        """Emit brief.missing only to subscribers that named the event.

        ``*`` subscribers opt out of this noisy per-missing-call signal; a
        subscriber asks for it by listing ``brief.missing`` in tokens/tokens.json.
        """
        if self.bus is None:
            return
        try:
            wanted = any(sub.get("active", True) and "brief.missing" in sub.get("events", [])
                         for sub in self.bus.subscriptions())
            if wanted:
                self.bus.emit("brief.missing", {"principal": principal, "tool": tool,
                                                "status": status, "mode": cfg["mode"],
                                                "window_min": cfg["window_min"]})
        except Exception:
            pass  # optional signal; never fail the guarded call over it

    def guarded(self, tool, principal, call):
        cfg = self.config()
        now = time.time()
        principal = str(principal or "").strip()
        records = self._records()
        last = self._last_brief(principal, records) if principal else None
        status = "unidentified" if not principal else (
            "fresh" if last is not None and (now - last) <= cfg["window_min"] * 60 else "missing")
        missing = status != "fresh"
        if missing:
            self._emit_missing(principal, tool, status, cfg)
            if cfg["mode"] == "block":
                self._append({"ts": _iso(now), "epoch": round(now, 3), "principal": principal,
                              "event": "call", "tool": tool, "brief": status,
                              "mode": cfg["mode"], "action": "blocked"})
                raise BriefRequired(self._message(principal, status, cfg))
        try:
            result = call()
        except Exception:
            self._append({"ts": _iso(now), "epoch": round(now, 3), "principal": principal,
                          "event": "call", "tool": tool, "brief": status,
                          "mode": cfg["mode"], "action": "error"})
            raise
        self._append({"ts": _iso(now), "epoch": round(now, 3), "principal": principal,
                      "event": "call", "tool": tool, "brief": status,
                      "mode": cfg["mode"], "action": "ok"})
        if not missing:
            return result
        warning = self._warning(principal, status, cfg, last)
        if isinstance(result, dict):
            return {**result, "brief_guard": warning}
        return {"result": result, "brief_guard": warning}

    def audit(self, hours=24):
        cfg = self.config()
        now = time.time()
        cutoff = now - hours * 3600
        latest, active, missing, calls = {}, {}, {}, {}
        for record in self._records():
            principal = str(record.get("principal") or "") or UNIDENTIFIED
            epoch = _epoch(record)
            if epoch is None:
                continue
            if record.get("event") == "brief":
                if principal not in latest or epoch > latest[principal]:
                    latest[principal] = epoch
                if epoch >= cutoff:
                    active[principal] = True
            elif record.get("event") == "call":
                if epoch < cutoff:
                    continue
                active[principal] = True
                calls[principal] = calls.get(principal, 0) + 1
                if record.get("brief") in ("missing", "unidentified"):
                    missing[principal] = missing.get(principal, 0) + 1
        return {"mode": cfg["mode"], "window_min": cfg["window_min"],
                "generated_at": _iso(now),
                "principals": {principal: {"last_brief": _iso(latest[principal]) if principal in latest else None,
                                           "missing_calls_24h": missing.get(principal, 0),
                                           "calls_24h": calls.get(principal, 0)}
                               for principal in sorted(active)}}
