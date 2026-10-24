#!/usr/bin/env python3
"""Three self-improvement loops for kiwi-live (ReasoningBank/Reflexion, GEPA/TextGrad, harness feedback).

1. lessons -> rule proposals: after ``record_lesson`` groups of similar lessons
   append an append-only proposal under ``inbox/rule-proposals/`` and emit
   ``rule.proposed``.  ``rules/`` is only ever read.
2. brief feedback: ``brief_feedback`` appends JSONL under ``logs/brief-feedback/``;
   ``context_for`` reorders its selected context with the accumulated
   useful-vs-noise score (bounded shift) and surfaces repeated ``missing`` text.
3. decide/ask calibration: decide predictions are stored in the same asks/
   predictions tables as ``ask`` so ``ask_resolve`` resolves them; the
   confidence threshold moves through ``token_set`` (token.changed) once enough
   outcomes exist, bounded by ``self_improve.calibration`` tokens.

Thresholds, weights and bounds live in ``tokens/tokens.json`` under
``self_improve``.  The constants below are fallbacks for a repo without that
key, never the primary source.
"""
import bisect
import datetime
import difflib
import json
import os
import re
import threading
import unicodedata
from pathlib import Path

from kiwi_events import SECRET_LINE, SEOUL

# Fallback defaults only; tokens/tokens.json `self_improve` overrides them.
RULE_DEFAULTS = {
    "min_group": 3,
    "similarity": 0.4,
    "update_overlap": 0.5,
    "default_rule": "rules/lessons.md",
    "area_rules": {"dev": "rules/code-review.md", "code": "rules/code-review.md",
                   "all": "rules/agent-procedure.md", "general": "rules/lessons.md"},
    "ignore_status": ["retired"],
}
BRIEF_DEFAULTS = {
    "useful_weight": 1.0,
    "noise_weight": 1.0,
    "max_score": 5.0,
    "max_shift": 2,
    "missing_min_count": 3,
    "days": 30,
}
CALIBRATION_DEFAULTS = {
    "threshold": 0.75,
    "threshold_min": 0.5,
    "threshold_max": 0.95,
    "step": 0.05,
    "min_samples": 20,
    "min_bucket": 3,
    "target_accuracy": 0.9,
    "buckets": [0.0, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0],
    "days": 30,
}
LOCK = threading.RLock()
SAFE_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,100}$")
PROPOSAL_COMMENT = re.compile(r"<!-- kiwi-proposal (\{.*?\}) -->")


def _now():
    return datetime.datetime.now(SEOUL).isoformat(timespec="seconds")


def _day():
    return datetime.datetime.now(SEOUL).date().isoformat()


def _num(value, default, low=None, high=None):
    try:
        value = float(value)
    except (TypeError, ValueError):
        value = float(default)
    if low is not None:
        value = max(low, value)
    if high is not None:
        value = min(high, value)
    return value


def _int(value, default, low=0, high=10 ** 6):
    try:
        value = int(value)
    except (TypeError, ValueError):
        value = int(default)
    return max(low, min(high, value))


def _slug(text, limit=80):
    text = unicodedata.normalize("NFC", str(text).strip().casefold())
    slug = re.sub(r"-+", "-", "".join(ch if ch.isalnum() else "-" for ch in text)).strip("-")
    return (slug or "proposal")[:limit].strip("-") or "proposal"


def _self_tokens(root):
    try:
        data = json.loads((Path(root) / "tokens" / "tokens.json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    section = data.get("self_improve") if isinstance(data, dict) else None
    return section if isinstance(section, dict) else {}


def _section(root, name):
    value = _self_tokens(root).get(name)
    return value if isinstance(value, dict) else {}


def _clean(text, limit=300):
    text = " ".join(str(text or "").split())
    text = "\n".join("[redacted]" if SECRET_LINE.search(line) else line for line in text.splitlines())
    return text[:limit]


# ---------------------------------------------------------------------------
# Loop 1: lessons -> append-only rule proposals
# ---------------------------------------------------------------------------

_HEADING = re.compile(r"^#{1,6}[ \t]+(.+?)\s*$", re.M)
_WORD = re.compile(r"[0-9a-z가-힣_]{2,}")
_STOP = set("the a an and or to of in on for with is are be been this that it as by from at into not no do "
            "does did but if then than so we you i my our your use used using run ran make made".split())


def _normalise(text):
    return "".join(str(text or "").casefold().split())


def _words(text):
    return {word for word in _WORD.findall(str(text or "").casefold()) if word not in _STOP}


def _frontmatter(text):
    lines = text.splitlines(keepends=True)
    if not lines or lines[0].strip() != "---":
        return {}, text
    for index in range(1, len(lines)):
        if lines[index].strip() == "---":
            fields = {}
            for line in lines[1:index]:
                key, _, value = line.partition(":")
                if key.strip():
                    fields[key.strip()] = value.strip().strip('"')
            return fields, "".join(lines[index + 1:])
    return {}, text


def _correct_section(body):
    match = None
    for name in ("다음엔", "이제", "correct", "fix"):
        match = re.search(r"^##[ \t]*" + re.escape(name) + r"[ \t\r]*$", body, re.M)
        if match:
            break
    if not match:
        return ""
    tail = body[match.end():]
    following = re.search(r"^##[ \t]+", tail, re.M)
    return (tail[:following.start()] if following else tail).strip()


def _safe_rule(root, value):
    value = str(value or "").strip().replace("\\", "/").removeprefix("./")
    if not value or ".." in value.split("/") or not value.startswith("rules/") or not value.endswith(".md"):
        return None
    if not (Path(root) / value).is_file():
        return None
    return value


class Lesson:
    __slots__ = ("path", "rel", "title", "area", "status", "rule", "correct", "tokens")

    def __init__(self, path, root):
        self.path = path
        self.rel = path.relative_to(root).as_posix()
        text = path.read_text(encoding="utf-8", errors="replace")
        fields, body = _frontmatter(text)
        heading = _HEADING.search(body)
        self.title = _clean(heading.group(1) if heading else path.stem, 200)
        self.area = _clean(fields.get("area") or "general", 80).casefold() or "general"
        self.status = _clean(fields.get("status") or "open", 40).casefold() or "open"
        self.rule = fields.get("rule") or fields.get("promoted_to") or ""
        self.correct = _clean(_correct_section(body), 300)
        self.tokens = _words(self.title) | _words(self.correct)


class RuleProposals:
    DIR = "inbox/rule-proposals"

    def __init__(self, root, bus=None):
        self.root = Path(root).resolve()
        self.bus = bus

    def config(self):
        cfg = {**RULE_DEFAULTS, **_section(self.root, "rule_proposals")}
        cfg["min_group"] = _int(cfg.get("min_group"), RULE_DEFAULTS["min_group"], 1, 1000)
        cfg["similarity"] = _num(cfg.get("similarity"), RULE_DEFAULTS["similarity"], 0.0, 1.0)
        cfg["update_overlap"] = _num(cfg.get("update_overlap"), RULE_DEFAULTS["update_overlap"], 0.0, 1.0)
        area_rules = cfg.get("area_rules") if isinstance(cfg.get("area_rules"), dict) else {}
        merged = dict(RULE_DEFAULTS["area_rules"])
        merged.update({str(k).casefold(): v for k, v in area_rules.items()})
        cfg["area_rules"] = merged
        cfg["default_rule"] = str(cfg.get("default_rule") or RULE_DEFAULTS["default_rule"])
        ignore = cfg.get("ignore_status")
        cfg["ignore_status"] = {str(x).casefold() for x in ignore} if isinstance(ignore, list) else set(RULE_DEFAULTS["ignore_status"])
        return cfg

    def _lessons(self, cfg):
        folder = self.root / "lessons"
        if not folder.is_dir():
            return []
        out = []
        for path in sorted(folder.glob("*.md")):
            if path.name == "README.md":
                continue
            try:
                lesson = Lesson(path, self.root)
            except OSError:
                continue
            if lesson.status not in cfg["ignore_status"]:
                out.append(lesson)
        return out

    def rule_for(self, lesson, cfg):
        explicit = _safe_rule(self.root, lesson.rule)
        if explicit:
            return explicit
        area = lesson.area.strip().casefold()
        target = cfg["area_rules"].get(area)
        if not target:
            candidate = "rules/" + area.replace("_", "-") + ".md"
            if _safe_rule(self.root, candidate):
                target = candidate
        return _safe_rule(self.root, target) or _safe_rule(self.root, cfg["default_rule"]) or "rules/lessons.md"

    @staticmethod
    def similarity(left, right):
        jaccard = len(left.tokens & right.tokens) / max(1, len(left.tokens | right.tokens))
        title = difflib.SequenceMatcher(None, _normalise(left.title), _normalise(right.title)).ratio()
        return max(jaccard, title)

    def _groups(self, lessons, cfg):
        parent = list(range(len(lessons)))

        def find(index):
            while parent[index] != index:
                parent[index] = parent[parent[index]]
                index = parent[index]
            return index

        def union(left, right):
            a, b = find(left), find(right)
            if a != b:
                parent[b] = a

        for left in range(len(lessons)):
            for right in range(left + 1, len(lessons)):
                if self.similarity(lessons[left], lessons[right]) >= cfg["similarity"]:
                    union(left, right)
        groups = {}
        for index in range(len(lessons)):
            groups.setdefault(find(index), []).append(lessons[index])
        out = []
        for members in groups.values():
            counts = {}
            for lesson in members:
                target = self.rule_for(lesson, cfg)
                counts[target] = counts.get(target, 0) + 1
            # Mixed areas: the group amends the rule most of its lessons point at.
            target = sorted(counts.items(), key=lambda item: (-item[1], item[0]))[0][0]
            out.append((target, sorted(members, key=lambda lesson: lesson.rel)))
        return sorted(out, key=lambda item: (item[0], item[1][0].rel))

    def _path(self, rule):
        return self.root / self.DIR / (_slug(rule[:-3] if rule.endswith(".md") else rule) + ".md")

    @staticmethod
    def _records(text):
        records = {}
        for match in PROPOSAL_COMMENT.finditer(text):
            try:
                record = json.loads(match.group(1))
            except ValueError:
                continue
            if isinstance(record, dict) and record.get("group"):
                records[record["group"]] = record
        return records

    @staticmethod
    def _match(records, members, overlap):
        best = None
        for group, record in records.items():
            old = set(record.get("lessons") or [])
            union = old | members
            score = len(old & members) / len(union) if union else 0.0
            if score >= overlap and (best is None or score > best[0]):
                best = (score, group, record)
        return best[1:] if best else (None, None)

    def _write(self, rule, members, cfg):
        path = self._path(rule)
        text = path.read_text(encoding="utf-8") if path.is_file() else ""
        records = self._records(text)
        rels = [lesson.rel for lesson in members]
        member_set = set(rels)
        group, record = self._match(records, member_set, cfg["update_overlap"])
        if record is not None and set(record.get("lessons") or []) == member_set:
            return None
        if record is None:
            group = "g" + str(max([int(g[1:]) for g in records if g.startswith("g") and g[1:].isdigit()] or [0]) + 1)
            version, added = 1, rels
        else:
            version = _int(record.get("version"), 0, 0, 10 ** 6) + 1
            added = sorted(member_set - set(record.get("lessons") or []))
        created = _now()
        lines = []
        if not text:
            lines += [f"# Rule proposal: {rule}", "",
                      "- generated_by: kiwi-live self_improve",
                      f"- would amend: `{rule}`",
                      "- rule_status: read-only; this loop never edits or deletes rules/",
                      ""]
        lines += [f"## group {group} · v{version} · {created}", "",
                  f"- rule: `{rule}`",
                  f"- lessons: {len(rels)}"]
        if added and version > 1:
            lines.append("- added: " + ", ".join(f"`{rel}`" for rel in added))
        lines.append("- cites:")
        for lesson in members:
            suggestion = re.sub(r"^[-*+]\s+", "", lesson.correct or lesson.title)
            lines.append(f"  - `{lesson.rel}` — {suggestion}")
        lines += ["- suggested change: add one line to the rule above that encodes the shared correction; "
                  "keep the rule file untouched until a human merges the proposal.", ""]
        machine = {"group": group, "version": version, "lessons": rels, "added": added,
                   "rule": rule, "count": len(rels), "created": created}
        lines += ["<!-- kiwi-proposal " + json.dumps(machine, ensure_ascii=False, sort_keys=True) + " -->", ""]
        path.parent.mkdir(parents=True, exist_ok=True)
        with LOCK:
            with path.open("a", encoding="utf-8", newline="") as handle:
                handle.write("\n".join(lines) + "\n")
        rel = path.relative_to(self.root).as_posix()
        event = None
        if self.bus is not None:
            try:
                event = self.bus.emit("rule.proposed", {"proposal": rel, "rule": rule, "group": group,
                                                        "version": version, "lessons": rels,
                                                        "added": added, "count": len(rels)})
            except Exception:
                event = None
        return {"proposal": rel, "rule": rule, "group": group, "version": version,
                "lessons": rels, "added": added, "event": event}

    def generate(self, after=None):
        with LOCK:
            cfg = self.config()
            groups = self._groups(self._lessons(cfg), cfg)
            written, seen = [], 0
            for rule, members in groups:
                if len(members) < cfg["min_group"]:
                    continue
                seen += 1
                changed = self._write(rule, members, cfg)
                if changed:
                    written.append(changed)
            return {"after": after, "groups": seen, "written": written}

    def after_lesson(self, rel=None):
        return self.generate(after=rel)

    def list(self, limit=20):
        folder = self.root / self.DIR
        limit = _int(limit, 20, 1, 200)
        out = []
        if not folder.is_dir():
            return out
        for path in sorted(folder.glob("*.md")):
            try:
                text = path.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            records = list(self._records(text).values())
            if not records:
                continue
            latest = max(records, key=lambda record: (_int(record.get("version"), 0), str(record.get("created", ""))))
            out.append({"path": path.relative_to(self.root).as_posix(),
                        "rule": latest.get("rule", ""),
                        "lessons": sorted({rel for record in records for rel in record.get("lessons") or []}),
                        "groups": [{"group": record.get("group"), "version": record.get("version"),
                                    "count": record.get("count"), "created": record.get("created")}
                                   for record in sorted(records, key=lambda item: str(item.get("group")))],
                        "created": records[0].get("created"), "updated": latest.get("created")})
        out.sort(key=lambda item: item["path"])
        return out[:limit]


# ---------------------------------------------------------------------------
# Loop 2: brief quality feedback
# ---------------------------------------------------------------------------

def _ids(value, name):
    if value is None:
        return []
    if isinstance(value, str):
        value = [part for part in re.split(r"[,\n]", value)]
    if not isinstance(value, (list, tuple)):
        raise ValueError(f"{name} must be a list of ids")
    out = []
    for item in value:
        text = str(item or "").strip()
        if not text:
            continue
        if len(text) > 200 or any(ord(ch) < 0x20 for ch in text):
            raise ValueError(f"bad {name} id")
        if text not in out:
            out.append(text)
    return out


class BriefFeedback:
    DIR = "logs/brief-feedback"

    def __init__(self, root, bus=None):
        self.root = Path(root).resolve()
        self.bus = bus

    def config(self):
        cfg = {**BRIEF_DEFAULTS, **_section(self.root, "brief_feedback")}
        cfg["useful_weight"] = _num(cfg.get("useful_weight"), BRIEF_DEFAULTS["useful_weight"], 0.0, 100.0)
        cfg["noise_weight"] = _num(cfg.get("noise_weight"), BRIEF_DEFAULTS["noise_weight"], 0.0, 100.0)
        cfg["max_score"] = _num(cfg.get("max_score"), BRIEF_DEFAULTS["max_score"], 0.0, 10 ** 6)
        cfg["max_shift"] = _int(cfg.get("max_shift"), BRIEF_DEFAULTS["max_shift"], 0, 1000)
        cfg["missing_min_count"] = _int(cfg.get("missing_min_count"), BRIEF_DEFAULTS["missing_min_count"], 1, 10 ** 6)
        cfg["days"] = _int(cfg.get("days"), BRIEF_DEFAULTS["days"], 1, 3650)
        return cfg

    def _rows(self, days=None, principal=None):
        cfg = self.config()
        days = _int(cfg["days"] if days is None else days, cfg["days"], 1, 3650)
        cutoff = datetime.datetime.now(datetime.timezone.utc).timestamp() - days * 86400
        rows = []
        folder = self.root / self.DIR
        if not folder.is_dir():
            return rows
        for path in sorted(folder.glob("*.jsonl")):
            try:
                with path.open(encoding="utf-8", errors="replace") as handle:
                    for line in handle:
                        try:
                            row = json.loads(line)
                        except ValueError:
                            continue
                        if not isinstance(row, dict):
                            continue
                        epoch = row.get("epoch")
                        if not isinstance(epoch, (int, float)):
                            try:
                                epoch = datetime.datetime.fromisoformat(str(row.get("ts"))).timestamp()
                            except (TypeError, ValueError):
                                continue
                        if epoch < cutoff:
                            continue
                        if principal and row.get("principal") != principal:
                            continue
                        rows.append(row)
            except OSError:
                continue
        return rows

    @staticmethod
    def _scores(rows, cfg):
        scores = {}
        for row in rows:
            for item in _ids(row.get("useful"), "useful"):
                scores[item] = scores.get(item, 0.0) + cfg["useful_weight"]
            for item in _ids(row.get("noise"), "noise"):
                scores[item] = scores.get(item, 0.0) - cfg["noise_weight"]
        return {item: round(max(-cfg["max_score"], min(cfg["max_score"], score)), 4)
                for item, score in scores.items()}

    def scores(self, principal=None, days=None):
        cfg = self.config()
        rows = self._rows(days, principal)
        if principal and not rows:
            rows = self._rows(days)
        return self._scores(rows, cfg)

    @staticmethod
    def _missing_counts(rows):
        counts = {}
        for row in rows:
            text = " ".join(str(row.get("missing") or "").split())
            if text:
                counts[text] = counts.get(text, 0) + 1
        return counts

    def missing_repeated(self, principal=None, days=None):
        cfg = self.config()
        rows = self._rows(days, principal)
        if principal and not rows:
            rows = self._rows(days)
        return [{"text": text, "count": count} for text, count in
                sorted(self._missing_counts(rows).items(), key=lambda item: (-item[1], item[0]))
                if count >= cfg["missing_min_count"]]

    def record(self, principal, bundle_id="", task="", useful=None, noise=None, missing="", note=""):
        principal = str(principal or "").strip()
        if not SAFE_ID.match(principal):
            raise ValueError("principal must be an agent id or account name")
        useful = _ids(useful, "useful")
        noise = _ids(noise, "noise")
        if not (useful or noise or str(missing or "").strip() or str(note or "").strip()):
            raise ValueError("feedback needs at least one of useful, noise, missing, note")
        try:  # reject secrets/student data before scrubbing, like record_lesson
            import kiwi_lessons
            kiwi_lessons._reject(str(bundle_id or ""), str(task or ""), str(missing or ""),
                                 str(note or ""), *useful, *noise)
        except ImportError:
            pass
        bundle_id = _clean(bundle_id, 200)
        task = _clean(task, 500)
        missing = _clean(missing, 2000)
        note = _clean(note, 2000)
        row = {"ts": _now(), "epoch": round(datetime.datetime.now(datetime.timezone.utc).timestamp(), 3),
               "principal": principal, "bundle_id": bundle_id, "task": task,
               "useful": useful, "noise": noise, "missing": missing, "note": note}
        path = self.root / self.DIR / (_day() + ".jsonl")
        path.parent.mkdir(parents=True, exist_ok=True)
        with LOCK:
            descriptor = os.open(path, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o644)
            with os.fdopen(descriptor, "a", encoding="utf-8") as handle:
                handle.write(json.dumps(row, ensure_ascii=False, separators=(",", ":")) + "\n")
        return {"action": "appended", "path": path.relative_to(self.root).as_posix(),
                "scores": self.scores(principal)}

    def reorder(self, selected, principal=None, days=None):
        cfg = self.config()
        items = list(selected or [])
        rows = self._rows(days, principal)
        if principal and not rows:
            rows = self._rows(days)
        scores = self._scores(rows, cfg)
        if not items or not scores:
            return items, {}
        # Bounded bubble: an item may move at most max_shift places from where it
        # started, so accumulated feedback can nudge but never reshuffle the brief.
        shift = cfg["max_shift"]
        weight = [scores.get(str(item.get("id")), 0.0) for item in items]
        order = list(range(len(items)))
        for _ in range(len(items)):
            swapped = False
            for position in range(len(items) - 1):
                left, right = order[position], order[position + 1]
                if weight[left] >= weight[right]:
                    continue
                if abs((position + 1) - left) > shift or abs(position - right) > shift:
                    continue
                order[position], order[position + 1] = right, left
                swapped = True
            if not swapped:
                break
        result = [items[index] for index in order]
        moved = sum(1 for position, index in enumerate(order) if position != index)
        return result, {"applied": moved, "max_shift": shift, "items": len(scores),
                        "feedback": len(rows)}

    def quality(self, days=None):
        cfg = self.config()
        days = _int(cfg["days"] if days is None else days, cfg["days"], 1, 3650)
        rows = self._rows(days)
        items = {}
        for row in rows:
            for item in _ids(row.get("useful"), "useful"):
                entry = items.setdefault(item, {"useful": 0, "noise": 0})
                entry["useful"] += 1
            for item in _ids(row.get("noise"), "noise"):
                entry = items.setdefault(item, {"useful": 0, "noise": 0})
                entry["noise"] += 1
        scores = self._scores(rows, cfg)
        useful_total = noise_total = 0
        for item, entry in items.items():
            total = entry["useful"] + entry["noise"]
            entry["total"] = total
            entry["useful_rate"] = round(entry["useful"] / total, 4) if total else None
            entry["noise_rate"] = round(entry["noise"] / total, 4) if total else None
            entry["score"] = scores.get(item, 0.0)
            useful_total += entry["useful"]
            noise_total += entry["noise"]
        missing = [{"text": text, "count": count} for text, count in
                   sorted(self._missing_counts(rows).items(), key=lambda item: (-item[1], item[0]))]
        rated = useful_total + noise_total
        return {"days": days, "entries": len(rows), "items": dict(sorted(items.items())),
                "useful": useful_total, "noise": noise_total,
                "useful_rate": round(useful_total / rated, 4) if rated else None,
                "missing": missing, "missing_repeated": [item for item in missing
                                                         if item["count"] >= cfg["missing_min_count"]]}


# ---------------------------------------------------------------------------
# Loop 3: decide/ask calibration and threshold
# ---------------------------------------------------------------------------

class Calibration:
    TOKEN = "self_improve.calibration.threshold"

    def __init__(self, root, tokens, bus=None):
        self.root = Path(root).resolve()
        self.tokens = tokens
        self.bus = bus

    def config(self):
        cfg = {**CALIBRATION_DEFAULTS, **_section(self.root, "calibration")}
        cfg["threshold"] = _num(cfg.get("threshold"), CALIBRATION_DEFAULTS["threshold"], 0.0, 1.0)
        cfg["threshold_min"] = _num(cfg.get("threshold_min"), CALIBRATION_DEFAULTS["threshold_min"], 0.0, 1.0)
        cfg["threshold_max"] = _num(cfg.get("threshold_max"), CALIBRATION_DEFAULTS["threshold_max"], 0.0, 1.0)
        if cfg["threshold_min"] > cfg["threshold_max"]:
            cfg["threshold_min"], cfg["threshold_max"] = cfg["threshold_max"], cfg["threshold_min"]
        cfg["step"] = _num(cfg.get("step"), CALIBRATION_DEFAULTS["step"], 0.0, 1.0)
        cfg["min_samples"] = _int(cfg.get("min_samples"), CALIBRATION_DEFAULTS["min_samples"], 1, 10 ** 6)
        cfg["min_bucket"] = _int(cfg.get("min_bucket"), CALIBRATION_DEFAULTS["min_bucket"], 1, 10 ** 6)
        cfg["target_accuracy"] = _num(cfg.get("target_accuracy"), CALIBRATION_DEFAULTS["target_accuracy"], 0.0, 1.0)
        cfg["days"] = _int(cfg.get("days"), CALIBRATION_DEFAULTS["days"], 1, 3650)
        edges = cfg.get("buckets")
        if not isinstance(edges, list) or len(edges) < 2:
            edges = CALIBRATION_DEFAULTS["buckets"]
        cleaned = []
        for edge in edges:
            try:
                edge = float(edge)
            except (TypeError, ValueError):
                continue
            if 0.0 <= edge <= 1.0 and (not cleaned or edge > cleaned[-1]):
                cleaned.append(edge)
        cfg["buckets"] = cleaned if len(cleaned) >= 2 else CALIBRATION_DEFAULTS["buckets"]
        return cfg

    def threshold(self):
        return float(self.config()["threshold"])

    @staticmethod
    def _bucket(edges, confidence):
        index = bisect.bisect_right(edges, confidence) - 1
        return max(0, min(len(edges) - 2, index))

    def report(self, days=None, reporter="self-improve", apply=True):
        import ask
        cfg = self.config()
        days = _int(cfg["days"] if days is None else days, cfg["days"], 1, 3650)
        data = ask.calibration(days)
        rows = data.get("rows") or []
        edges = cfg["buckets"]
        buckets = []
        for index in range(len(edges) - 1):
            buckets.append({"from": edges[index], "to": edges[index + 1], "total": 0, "hits": 0})
        for row in rows:
            try:
                confidence = float(row.get("confidence") or 0.0)
            except (TypeError, ValueError):
                confidence = 0.0
            bucket = buckets[self._bucket(edges, max(0.0, min(1.0, confidence)))]
            bucket["total"] += 1
            bucket["hits"] += 1 if row.get("hit") else 0
        for bucket in buckets:
            bucket["accuracy"] = round(bucket["hits"] / bucket["total"], 4) if bucket["total"] else None
        total = len(rows)
        hits = sum(1 for row in rows if row.get("hit"))
        current = cfg["threshold"]
        above = [row for row in rows if float(row.get("confidence") or 0.0) >= current]
        below = [row for row in rows if float(row.get("confidence") or 0.0) < current]
        above_hits = sum(1 for row in above if row.get("hit"))
        below_hits = sum(1 for row in below if row.get("hit"))
        above_accuracy = above_hits / len(above) if above else None
        below_accuracy = below_hits / len(below) if below else None
        recommended, action, reason = current, "hold", "no_change"
        if total < cfg["min_samples"]:
            reason = "insufficient_evidence"
        elif above_accuracy is not None and len(above) >= cfg["min_bucket"] and above_accuracy < cfg["target_accuracy"] and current + cfg["step"] <= cfg["threshold_max"]:
            recommended, action, reason = round(min(cfg["threshold_max"], current + cfg["step"]), 4), "raise", "accuracy_below_target"
        elif below_accuracy is not None and len(below) >= cfg["min_bucket"] and below_accuracy >= cfg["target_accuracy"] and current - cfg["step"] >= cfg["threshold_min"]:
            recommended, action, reason = round(max(cfg["threshold_min"], current - cfg["step"]), 4), "lower", "lower_bucket_accurate"
        elif len(above) < cfg["min_bucket"] or len(below) < cfg["min_bucket"]:
            reason = "insufficient_side_evidence"
        change = None
        if apply and action in ("raise", "lower"):
            try:
                change = self.tokens.set(self.TOKEN, recommended, reporter, create=True,
                                         description="decide_calibration: " + reason + f" ({total} resolved, {days}d)")
            except Exception as error:  # tokens unavailable: report the recommendation, never lose the report
                change = {"error": str(error)[:300]}
        return {"days": days, "resolved": total, "hits": hits,
                "accuracy": round(hits / total, 4) if total else None,
                "should_move": action != "hold", "recommended_threshold": recommended,
                "changed": bool(isinstance(change, dict) and change.get("action") == "changed"),
                "buckets": buckets,
                "threshold": {"current": current, "recommended": recommended, "action": action,
                              "should_move": action != "hold", "reason": reason, "min_samples": cfg["min_samples"],
                              "min_bucket": cfg["min_bucket"], "target_accuracy": cfg["target_accuracy"],
                              "bounds": [cfg["threshold_min"], cfg["threshold_max"]], "step": cfg["step"]},
                "change": change}

    def summary(self):
        cfg = self.config()
        try:
            import ask
            data = ask.calibration(cfg["days"])
            rows = data.get("rows") or []
            hits = sum(1 for row in rows if row.get("hit"))
        except Exception:
            rows, hits = [], 0
        return {"threshold": self.threshold(),
                "resolved_30d": len(rows),
                "accuracy_30d": round(hits / len(rows), 4) if rows else None}


class SelfImprove:
    """Facade wired into Sprouts; keeps the three loops' storage side by side."""

    def __init__(self, root, bus=None, tokens=None):
        self.root = Path(root).resolve()
        self.bus = bus
        self.tokens = tokens
        self.proposals = RuleProposals(self.root, bus)
        self.feedback = BriefFeedback(self.root, bus)
        self.calibration = Calibration(self.root, tokens, bus)

    def summary(self):
        try:
            proposals = len(self.proposals.list(limit=200))
        except Exception:
            proposals = 0
        try:
            feedback = self.feedback.quality()["entries"]
        except Exception:
            feedback = 0
        try:
            calibration = self.calibration.summary()
        except Exception:
            calibration = {"threshold": None, "resolved_30d": 0, "accuracy_30d": None}
        return {"rule_proposals": proposals, "brief_feedback_30d": feedback,
                "threshold": calibration.get("threshold"),
                "calibration_resolved_30d": calibration.get("resolved_30d"),
                "calibration_accuracy_30d": calibration.get("accuracy_30d")}
