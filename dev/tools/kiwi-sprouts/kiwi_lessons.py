#!/usr/bin/env python3
"""Write Kiwi lessons and decision inbox lines with scrubbing, dedupe, and commits."""
import datetime, difflib, json, re, subprocess, sys, threading, unicodedata
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from kiwi_live import SECRET_LINE  # noqa: E402

LOCK = threading.Lock()
STUDENT_LINE = re.compile(r"\d{6}-[1-4]\d{6}|01\d-?\d{3,4}-?\d{4}|학생\s*이름|학번|student\s*name", re.I)
EMAIL = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")
PUBLIC_MAIL = {"example.com", "207studio.dev"}
SEOUL = datetime.timezone(datetime.timedelta(hours=9))


def _today():
    return datetime.datetime.now(SEOUL).date().isoformat()


def _required(name, value):
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{name} must be a non-empty string")
    return value.strip()


def _optional(name, value):
    if not isinstance(value, str):
        raise ValueError(f"{name} must be a string")
    return value.strip()


def _reject(*values):
    """Refuse secrets and student data before anything is written."""
    for value in values:
        text = str(value)
        if SECRET_LINE.search(text) or STUDENT_LINE.search(text):
            raise ValueError("refusing to record a secret or student data")
        for match in EMAIL.finditer(text):
            local, _, domain = match.group(0).partition("@")
            if local.lower() != "noreply" and domain.lower() not in PUBLIC_MAIL:
                raise ValueError("refusing to record student data: email address")


def _title(title, what):
    if title:
        return title
    first = what.splitlines()[0].strip()
    match = re.match(r"(.+?[.!?。！？])(?:\s|$)", first)
    return (match.group(1) if match else first).strip()


def _occurrence(when, where, reporter):
    return f"{when} {where} ({reporter} 보고)"


def _slug(text):
    text = unicodedata.normalize("NFC", text.strip().casefold())
    slug = re.sub(r"-+", "-", "".join(ch if ch.isalnum() else "-" for ch in text)).strip("-")
    return slug or "lesson"


def _frontmatter(text):
    lines = text.splitlines(keepends=True)
    if not lines or lines[0].strip() != "---":
        return None
    for index in range(1, len(lines)):
        if lines[index].strip() == "---":
            return lines, index
    return None


def _field(lines, end, key):
    for index in range(1, end):
        name, _, value = lines[index].partition(":")
        if name.strip() == key:
            return index, value.strip()
    return None, None


def _eol(line):
    if line.endswith("\r\n"):
        return "\r\n"
    if line.endswith("\n"):
        return "\n"
    if line.endswith("\r"):
        return "\r"
    return ""


def _json_list(raw):
    try:
        value = json.loads(raw)
    except (TypeError, ValueError):
        return []
    return value if isinstance(value, list) else []


def _body(lines, end):
    return "".join(lines[end + 1:])


def _heading(body):
    match = re.search(r"^#[ \t]+(.+?)\s*$", body, re.M)
    return match.group(1).strip() if match else ""


def _jiye(body):
    match = re.search(r"^##[ \t]*이제[ \t\r]*$", body, re.M)
    if not match:
        return ""
    tail = body[match.end():]
    following = re.search(r"^##[ \t]+", tail, re.M)
    return (tail[:following.start()] if following else tail).strip()


def _norm(text):
    return "".join(text.casefold().split())


def _lessons(root):
    folder = root / "lessons"
    return sorted(folder.glob("*.md")) if folder.is_dir() else []


def _same(root, title, correct):
    wanted = _norm(title)
    for path in _lessons(root):
        if path.name == "README.md":
            continue
        parsed = _frontmatter(path.read_text(encoding="utf-8"))
        lines, end = parsed if parsed else (path.read_text(encoding="utf-8").splitlines(keepends=True), 0)
        body = _body(lines, end)
        if wanted and _norm(_heading(body)) == wanted:
            return path
        section = _jiye(body)
        if section and difflib.SequenceMatcher(None, correct, section).ratio() >= 0.6:
            return path
    return None


def _append_occurrence(path, item):
    with path.open(encoding="utf-8", newline="") as handle:
        text = handle.read()
    parsed = _frontmatter(text)
    if not parsed:
        raise ValueError(f"lesson frontmatter missing: {path}")
    lines, end = parsed
    occurrence_index, raw = _field(lines, end, "occurrences")
    status_index, status = _field(lines, end, "status")
    if status_index is not None and status.strip('"') == "fixed":
        lines[status_index] = "status: open" + _eol(lines[status_index])
    occurrences = _json_list(raw) if occurrence_index is not None else []
    occurrences.append(item)
    line = "occurrences: " + json.dumps(occurrences, ensure_ascii=False)
    if occurrence_index is None:
        eol = _eol(lines[end]) or _eol(lines[0]) or "\n"
        lines.insert(end, line + eol)
    else:
        lines[occurrence_index] = line + _eol(lines[occurrence_index])
    path.write_text("".join(lines), encoding="utf-8", newline="")


def _new_path(root, day, slug):
    base = f"{day}-{slug}"
    path, number = root / "lessons" / f"{base}.md", 2
    while path.exists():
        path = root / "lessons" / f"{base}-{number}.md"
        number += 1
    return path


def _render(day, reporter, area, occurrence, title, what, where, correct):
    lines = ["---", f"date: {day}", f"recorded_by: {reporter}", f"area: {area or 'general'}",
             "status: open", "occurrences: " + json.dumps([occurrence], ensure_ascii=False), "---", "",
             f"# {title}", "", "## 무엇이", "", what]
    if where:
        lines += ["", f"근거: {where}"]
    lines += ["", "## 이제", "", "- " + correct]
    return "\n".join(lines) + "\n"


def _rel(root, path):
    return path.relative_to(root).as_posix()


def _git(root, *args):
    try:
        return subprocess.run(["git", "-C", str(root), *args], capture_output=True, text=True,
                              encoding="utf-8", errors="replace")
    except OSError as error:
        return subprocess.CompletedProcess(args, 127, "", str(error))


def _pull(root):
    _git(root, "pull", "--rebase", "--autostash", "-q")


def _restore(root, rel):
    path = root / rel
    if _git(root, "ls-files", "--error-unmatch", "--", rel).returncode == 0:
        _git(root, "checkout", "--", rel)
    elif path.exists():
        path.unlink()


def _run_scripts(root, rel):
    check = root / "scripts" / "check_assets.py"
    if check.is_file():
        result = subprocess.run([sys.executable, str(check)], cwd=str(root), capture_output=True, text=True,
                                encoding="utf-8", errors="replace")
        if result.returncode != 0:
            _restore(root, rel)
            raise RuntimeError("check_assets.py failed: " + (result.stdout + result.stderr).strip())
    build = root / "scripts" / "build_index.py"
    if build.is_file():
        result = subprocess.run([sys.executable, str(build)], cwd=str(root), capture_output=True, text=True,
                                encoding="utf-8", errors="replace")
        if result.returncode != 0:
            raise RuntimeError("build_index.py failed: " + (result.stdout + result.stderr).strip())


def _commit(root, rel, message, commit):
    if not commit:
        return None
    _git(root, "add", "--", rel)
    if _git(root, "diff", "--cached", "--quiet").returncode == 0:
        return _git(root, "rev-parse", "HEAD").stdout.strip() or None
    result = _git(root, "commit", "-m", message)
    if result.returncode != 0:
        raise RuntimeError("git commit failed: " + (result.stderr or result.stdout).strip())
    return _git(root, "rev-parse", "HEAD").stdout.strip()


def record_lesson(root, what, when, correct, reporter, where="", area="", title="", commit=True, events=None):
    root = Path(root).resolve()
    what = _required("what", what)
    when = _required("when", when)
    correct = _required("correct", correct)
    reporter = _required("reporter", reporter)
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,80}", reporter):
        raise ValueError("reporter must be an agent id or account name")
    where = _optional("where", where)
    area = _optional("area", area)
    title = _optional("title", title)
    _reject(what, when, where, correct, reporter, area, title)
    lesson_title = _title(title, what)
    item = _occurrence(when, where, reporter)
    with LOCK:
        if commit:
            _pull(root)
        found = _same(root, lesson_title, correct)
        if found:
            rel = _rel(root, found)
            _append_occurrence(found, item)
            if commit:
                _run_scripts(root, rel)
            message = f"lessons: {lesson_title} (record_lesson, {reporter})"
            result = {"action": "appended", "path": rel, "commit": _commit(root, rel, message, commit)}
        else:
            (root / "lessons").mkdir(parents=True, exist_ok=True)
            day = _today()
            path = _new_path(root, day, _slug(title or what[:60]))
            path.write_text(_render(day, reporter, area, item, lesson_title, what, where, correct),
                            encoding="utf-8", newline="")
            rel = _rel(root, path)
            if commit:
                _run_scripts(root, rel)
            message = f"lessons: {lesson_title} (record_lesson, {reporter})"
            result = {"action": "created", "path": rel, "commit": _commit(root, rel, message, commit)}
    # Event-driven loop 1: no polling/timer; every recorded lesson re-examines existing lessons.
    try:
        import kiwi_selfimprove
        result["rule_proposals"] = kiwi_selfimprove.RuleProposals(root, events).after_lesson(result["path"])
    except Exception as error:  # a proposal failure must never lose the lesson
        result["rule_proposals_error"] = str(error)[:300]
    return result


def record_decision(root, line, reporter, commit=True):
    root = Path(root).resolve()
    if isinstance(line, str) and ("\n" in line or "\r" in line):
        raise ValueError("line must be one line")
    line = _required("line", line)
    reporter = _required("reporter", reporter)
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,80}", reporter):
        raise ValueError("reporter must be an agent id or account name")
    _reject(line, reporter)
    day = _today()
    rel = f"logs/decisions-inbox/{day}-{reporter}.md"
    with LOCK:
        if commit:
            _pull(root)
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        if path.exists():
            with path.open(encoding="utf-8", newline="") as handle:
                text = handle.read()
        else:
            text = ""
        entry = "- " + line
        if entry in text.splitlines():
            return {"action": "skipped", "path": rel, "commit": None}
        if not text:
            text = f"# decisions inbox {day} {reporter}\n\n"
        elif not text.endswith(("\n", "\r")):
            text += "\n"
        path.write_text(text + entry + "\n", encoding="utf-8", newline="")
        return {"action": "appended", "path": rel,
                "commit": _commit(root, rel, f"logs: decision (record_decision, {reporter})", commit)}
