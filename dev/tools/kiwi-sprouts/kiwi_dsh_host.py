#!/usr/bin/env python3
"""Pick the host and coding model that run dsh, and report host load.

The source of truth is ``tokens/tokens.json`` -> ``dsh_host``.  Host names,
machine ids, thresholds and the ordered coding-model priority
(``coding_route.priority``) live there; the dictionary below is only the
fallback used when that key is missing or unreadable.

Usage::

    kiwi_dsh_host.py select [--mac-only] [--json]
    kiwi_dsh_host.py report --host <id>

``select`` returns both ``host`` and ``model`` with reasons.  Remote host
candidates are read from ``$KIWI_HOME/host-load/<id>.json`` (default
``~/.kiwi/host-load``), where the kiwi-live ``POST /host-load`` endpoint stores
pushed reports.  Coding-model usage is read from
``$KIWI_HOME/usage/<model>.json`` (default ``~/.kiwi/usage``), a file or token
flag that marks a model exhausted.  ``report`` measures the current machine
without third-party modules and pushes the result to kiwi-live using
``KIWI_LIVE_URL`` and ``KIWI_LIVE_TOKEN``.
"""
from __future__ import annotations

import argparse
import json
import os
import platform
import re
import subprocess
import sys
import time
import urllib.request
from datetime import datetime, timedelta, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
KIWI_ROOT = Path(os.environ.get("KIWI_ROOT") or HERE.parents[2])
SEOUL = timezone(timedelta(hours=9))
HOST_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")

# Fallback only.  Change the values in tokens/tokens.json, not here.
FALLBACK = {
    "description": "dsh host and coding-model routing",
    "coding_route": {
        "usage_dir": "~/.kiwi/usage",
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
        "note": "Account used for automatic code review; bots do not change its settings.",
    },
    "candidates": [
        {
            "id": "windows-studio-pc",
            "tailscale": "studio-pc",
            "machine_id": "00000000-0000-4000-8000-6f5c571ba078",
            "local_host": "studio-pc",
            "ready": True,
            "note": "dsh 0.2.1-rc.2 installed and tested; active default for non-Mac-only coding",
        },
        {"id": "box", "tailscale": "box-host", "local": True, "note": "last-resort fallback"},
        {
            "id": "mac-mini",
            "tailscale": "mac-host",
            "machine_id": "00000000-0000-4000-8000-e361d915d2ec",
            "mac_only": True,
        },
    ],
    "mac_only_tasks": [
        "simulator",
        "Xcode final build",
        "App Store submission",
        "iPhone mirroring",
    ],
    "thresholds": {
        "cpu_percent": 85,
        "memory_percent": 85,
        "max_running_dsh": 2,
        "report_stale_seconds": 900,
    },
}


def _iso(epoch=None):
    return datetime.fromtimestamp(epoch if epoch is not None else time.time(), SEOUL).isoformat(timespec="seconds")


def _as_epoch(value):
    """Return an epoch for an epoch number or an ISO-8601 string."""
    if isinstance(value, bool):
        return None
    if isinstance(value, (int, float)):
        return float(value)
    if not isinstance(value, str):
        return None
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=SEOUL)
    return parsed.timestamp()


def _epoch(record):
    value = record.get("epoch")
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return float(value)
    return _as_epoch(record.get("measured_at"))


def _num(value):
    if isinstance(value, bool):
        return None
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


def load_config(root=None):
    """Read dsh_host from tokens/tokens.json and fill missing keys from FALLBACK."""
    path = Path(root or os.environ.get("KIWI_ROOT") or KIWI_ROOT) / "tokens" / "tokens.json"
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        data = {}
    raw = data.get("dsh_host") if isinstance(data, dict) else None
    config = json.loads(json.dumps(FALLBACK))
    if not isinstance(raw, dict):
        return config
    if isinstance(raw.get("description"), str):
        config["description"] = raw["description"]
    if isinstance(raw.get("coding_route"), dict):
        route = raw["coding_route"]
        if isinstance(route.get("usage_dir"), str) and route["usage_dir"].strip():
            config["coding_route"]["usage_dir"] = route["usage_dir"]
        if isinstance(route.get("priority"), list) and route["priority"]:
            config["coding_route"]["priority"] = [
                dict(item) for item in route["priority"]
                if isinstance(item, dict) and item.get("id")
            ]
    if isinstance(raw.get("code_review_account"), dict):
        config["code_review_account"] = {
            str(k): str(v) for k, v in raw["code_review_account"].items()
        }
    if isinstance(raw.get("candidates"), list) and raw["candidates"]:
        config["candidates"] = [dict(item) for item in raw["candidates"] if isinstance(item, dict) and item.get("id")]
    if isinstance(raw.get("mac_only_tasks"), list):
        config["mac_only_tasks"] = [str(item) for item in raw["mac_only_tasks"]]
    if isinstance(raw.get("thresholds"), dict):
        for key in config["thresholds"]:
            if key in raw["thresholds"]:
                config["thresholds"][key] = raw["thresholds"][key]
    return config


def reports_dir(home=None):
    base = Path(os.path.expanduser(home or os.environ.get("KIWI_HOME") or "~/.kiwi"))
    return base / "host-load"


def usage_dir(home=None):
    base = Path(os.path.expanduser(home or os.environ.get("KIWI_HOME") or "~/.kiwi"))
    return base / "usage"


def _report_path(host_id, directory=None):
    if not HOST_ID.match(str(host_id)):
        raise ValueError("invalid host id")
    return Path(directory or reports_dir()) / (str(host_id) + ".json")


def read_report(host_id, directory=None):
    try:
        path = _report_path(host_id, directory)
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    return data if isinstance(data, dict) else None


def write_report(host_id, record, directory=None):
    """Validate and atomically store one load report."""
    rec = dict(record)
    for key in ("cpu_percent", "memory_percent"):
        value = _num(rec.get(key))
        if value is None:
            raise ValueError(f"missing numeric {key}")
        rec[key] = round(value, 2)
    value = _num(rec.get("running_dsh"))
    if value is None or value < 0:
        raise ValueError("missing numeric running_dsh")
    rec["running_dsh"] = int(value)
    now = time.time()
    rec["host"] = str(host_id)
    rec.setdefault("measured_at", _iso(now))
    rec.setdefault("epoch", round(now, 3))
    path = _report_path(host_id, directory)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(rec, ensure_ascii=False, sort_keys=True) + "\n", encoding="utf-8")
    os.replace(temporary, path)
    return path


def _usage_path(entry, directory):
    """Resolve an entry's usage report under the configured usage directory."""
    name = entry.get("usage_report")
    if name:
        path = Path(os.path.expanduser(str(name)))
        return path if path.is_absolute() else Path(directory) / path
    entry_id = str(entry.get("id") or "")
    if not HOST_ID.match(entry_id):
        raise ValueError("invalid coding route id")
    return Path(directory) / (entry_id + ".json")


def _read_usage(entry, directory):
    try:
        path = _usage_path(entry, directory)
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    return data if isinstance(data, dict) else None


def usage_state(entry, directory, now):
    """Return (available, reason, usage_record) for one coding-route entry.

    A missing report means "not reported yet" and is treated as available so a
    fresh install does not block coding.  A report can mark the model exhausted
    with its configured flag, ``available:false``, zero remaining usage, or a
    future reset time.  A past reset time makes the model available again.
    """
    if entry.get("exhausted") is True or entry.get("usage_exhausted") is True:
        return False, "token flag says exhausted", None
    flag_field = str(entry.get("exhausted_field") or "exhausted")
    record = _read_usage(entry, directory)
    if record is None:
        return True, "no usage report (assumed available)", None
    exhausted = record.get(flag_field) is True
    for key in ("exhausted", "limit_reached", "quota_exhausted", "out_of_usage"):
        if record.get(key) is True:
            exhausted = True
    if record.get("available") is False:
        exhausted = True
    remaining_value = record.get("remaining_percent")
    if remaining_value is None:
        remaining_value = record.get("remaining")
    remaining = _num(remaining_value)
    if remaining is not None and remaining <= 0:
        exhausted = True
    used_value = record.get("usage_percent")
    if used_value is None:
        used_value = record.get("used_percent")
    used = _num(used_value)
    if used is not None and used >= 100:
        exhausted = True
    status = str(record.get("status") or "").strip().lower()
    if status in ("exhausted", "limit_reached", "out_of_usage", "quota_exhausted"):
        exhausted = True
    reset = _as_epoch(record.get("exhausted_until"))
    if reset is not None and reset > now:
        exhausted = True
    if reset is None and exhausted:
        reset = _as_epoch(record.get("resets_at") or record.get("reset_at"))
    if exhausted:
        if reset is not None and reset <= now:
            return True, f"usage reset at {_iso(reset)}", record
        if reset is not None:
            return False, f"exhausted until {_iso(reset)}", record
        return False, "usage report says exhausted", record
    return True, "usage available", record


def select_model(config, directory, now):
    """Pick the first coding model whose usage is not exhausted."""
    route = config.get("coding_route") if isinstance(config.get("coding_route"), dict) else {}
    entries = [entry for entry in route.get("priority", []) if isinstance(entry, dict) and entry.get("id")]
    notes = []
    decisions = []
    if not entries:
        return None, "coding_route.priority is empty", decisions
    chosen = None
    chosen_why = ""
    for entry in entries:
        available, why, record = usage_state(entry, directory, now)
        decisions.append({
            "id": str(entry["id"]),
            "model": entry.get("model") or entry["id"],
            "effort": entry.get("effort"),
            "tool": entry.get("tool"),
            "available": available,
            "reason": why,
            "usage": record,
        })
        if available and chosen is None:
            chosen = entry
            chosen_why = why
        elif not available:
            notes.append(f"{entry['id']} skipped: {why}")
    if chosen is not None:
        reason = "; ".join(notes + [f"{chosen['id']} available: {chosen_why}"])
        return chosen, reason, decisions
    return None, "; ".join(notes + ["all coding models exhausted"]), decisions


def _run(command, timeout=10):
    return subprocess.run(
        command, capture_output=True, text=True, encoding="utf-8", errors="replace",
        timeout=timeout, check=True,
    ).stdout


def _cpu_percent():
    system = platform.system()
    if system == "Windows":
        command = ("(Get-CimInstance Win32_Processor | "
                   "Measure-Object -Property LoadPercentage -Average).Average")
        try:
            value = float(_run(["powershell", "-NoProfile", "-NonInteractive", "-Command", command]).strip())
            return max(0.0, value)
        except (OSError, ValueError, subprocess.SubprocessError):
            return 0.0
    try:
        load = float(Path("/proc/loadavg").read_text(encoding="utf-8").split()[0])
    except (OSError, ValueError, IndexError):
        try:
            load = float(os.getloadavg()[0])
        except (AttributeError, OSError, ValueError):
            return 0.0
    return max(0.0, load / max(1, os.cpu_count() or 1) * 100.0)


def _linux_memory_percent():
    values = {}
    for line in Path("/proc/meminfo").read_text(encoding="utf-8").splitlines():
        if ":" not in line:
            continue
        key, rest = line.split(":", 1)
        match = re.search(r"\d+", rest)
        if match:
            values[key.strip()] = int(match.group())
    total = values.get("MemTotal", 0)
    available = values.get("MemAvailable", values.get("MemFree", 0))
    return (total - available) / total * 100.0 if total else 0.0


def _darwin_memory_percent():
    total = int(_run(["sysctl", "-n", "hw.memsize"]).strip())
    output = _run(["vm_stat"])
    page_size = 4096
    match = re.search(r"page size of (\d+) bytes", output)
    if match:
        page_size = int(match.group(1))
    pages = {}
    for line in output.splitlines():
        if ":" not in line:
            continue
        key, rest = line.split(":", 1)
        match = re.search(r"\d+", rest)
        if match:
            pages[key.strip()] = int(match.group())
    free = sum(pages.get(key, 0) for key in ("Pages free", "Pages inactive", "Pages speculative"))
    if total <= 0:
        return 0.0
    return max(0.0, min(100.0, (total - free * page_size) / total * 100.0))


def _windows_memory_percent():
    command = ("$os = Get-CimInstance Win32_OperatingSystem; "
               "$total = [double]$os.TotalVisibleMemorySize; "
               "$free = [double]$os.FreePhysicalMemory; "
               "if ($total -gt 0) { [math]::Round((1 - $free / $total) * 100, 2) } else { 0 }")
    value = float(_run(["powershell", "-NoProfile", "-NonInteractive", "-Command", command]).strip())
    return max(0.0, min(100.0, value))


def _memory_percent():
    system = platform.system()
    try:
        if system == "Linux":
            return _linux_memory_percent()
        if system == "Darwin":
            return _darwin_memory_percent()
        if system == "Windows":
            return _windows_memory_percent()
    except (OSError, ValueError, subprocess.SubprocessError):
        pass
    return 0.0


def _is_dsh(token):
    name = str(token).strip().strip('"').replace("\\", "/").rstrip("/").rsplit("/", 1)[-1].lower()
    return name in ("dsh", "dsh.exe", "dsh.cmd", "dsh.ps1") or name.startswith("dsh-")


def _count_dsh_lines(lines):
    count = 0
    for line in lines:
        if any(_is_dsh(token) for token in str(line).split()):
            count += 1
    return count


def _running_dsh():
    system = platform.system()
    if system == "Linux":
        count = 0
        try:
            processes = list(Path("/proc").iterdir())
        except OSError:
            return 0
        for process in processes:
            if not process.name.isdigit():
                continue
            try:
                command = (process / "cmdline").read_bytes().replace(b"\x00", b" ").decode("utf-8", "replace")
            except OSError:
                continue
            if any(_is_dsh(token) for token in command.split()):
                count += 1
        return count
    try:
        if system == "Darwin":
            lines = _run(["ps", "-axo", "args="]).splitlines()
        elif system == "Windows":
            command = "Get-CimInstance Win32_Process | Select-Object -ExpandProperty CommandLine"
            lines = _run(["powershell", "-NoProfile", "-NonInteractive", "-Command", command]).splitlines()
        else:
            return 0
    except (OSError, subprocess.SubprocessError):
        return 0
    return _count_dsh_lines(lines)


def measure_local_load(now=None):
    return {
        "cpu_percent": round(_cpu_percent(), 2),
        "memory_percent": round(_memory_percent(), 2),
        "running_dsh": _running_dsh(),
        "measured_at": _iso(now),
        "epoch": round(now if now is not None else time.time(), 3),
        "source": "local",
    }


def _over_threshold(load, thresholds):
    for key, label in (("cpu_percent", "cpu"), ("memory_percent", "memory")):
        value = _num(load.get(key))
        limit = _num(thresholds.get(key))
        if value is None:
            return f"{label} load missing"
        if limit is not None and value > limit:
            return f"{label} {value:g}% over {limit:g}% threshold"
    running = _num(load.get("running_dsh"))
    limit = _num(thresholds.get("max_running_dsh"))
    if running is None:
        return "running dsh count missing"
    if limit is not None and running > limit:
        return f"{int(running)} dsh running over max {int(limit)}"
    return None


def _is_local_candidate(candidate, hostname=None):
    if candidate.get("local"):
        return True
    local_name = candidate.get("local_host") or candidate.get("hostname")
    if not local_name:
        return False
    host = str(hostname if hostname is not None else platform.node()).strip().lower()
    return bool(host) and str(local_name).strip().lower() == host


def _candidate_load(candidate, config, directory, local_loader, now, hostname):
    host = candidate.get("id")
    if not candidate.get("ready", True):
        note = str(candidate.get("note") or "").strip()
        return None, "not ready" + (f": {note}" if note else "")
    if _is_local_candidate(candidate, hostname):
        try:
            load = local_loader()
        except Exception as error:  # local measurement must not break selection
            return None, f"local load unavailable: {error}"
        if not isinstance(load, dict):
            return None, "local load unavailable"
        load = dict(load)
        load.setdefault("measured_at", _iso(now))
        load.setdefault("epoch", round(now, 3))
        load.setdefault("source", "local")
    else:
        load = read_report(host, directory)
        if not isinstance(load, dict):
            if candidate.get("missing_report") == "assume_available":
                return {"source": "assumed", "note": "no load report"}, None
            return None, "no load report"
        measured = _epoch(load)
        if measured is None:
            return None, "load report has no timestamp"
        stale = _num(config["thresholds"].get("report_stale_seconds"))
        if stale is not None and now - measured > stale:
            return None, f"load report stale ({int(now - measured)}s > {int(stale)}s)"
    reason = _over_threshold(load, config["thresholds"])
    if reason:
        return None, reason
    return load, None


def _result(candidate, load, mac_only, reason, config, model=None, model_reason="", route=None):
    return {
        "host": candidate.get("id") if candidate else None,
        "id": candidate.get("id") if candidate else None,
        "tailscale": candidate.get("tailscale") if candidate else None,
        "machine_id": candidate.get("machine_id") if candidate else None,
        "mac_only": bool(mac_only),
        "model": model.get("id") if model else None,
        "model_name": (model.get("model") or model.get("id")) if model else None,
        "model_effort": model.get("effort") if model else None,
        "model_tool": model.get("tool") if model else None,
        "model_reason": model_reason,
        "route": route or [],
        "code_review_account": config.get("code_review_account", {}),
        "reason": reason,
        "load": load,
    }


def select_host(mac_only=False, root=None, reports=None, local_loader=None, now=None,
                usage=None, hostname=None):
    """Return the host and coding model to use now, with one reason each.

    The coding model is the first entry in ``dsh_host.coding_route.priority``
    whose usage is not exhausted.  Mac-only work goes to the first ready
    mac_only candidate.  All other work walks the candidate list in priority
    order and never selects a mac_only host.  If nothing is ready, fresh and
    under thresholds, host is None.
    """
    config = load_config(root)
    directory = Path(reports) if reports else reports_dir()
    route_config = config.get("coding_route") if isinstance(config.get("coding_route"), dict) else {}
    usage_directory = Path(usage) if usage else Path(os.path.expanduser(
        route_config.get("usage_dir") or str(usage_dir())
    ))
    now = time.time() if now is None else float(now)
    local_loader = local_loader or measure_local_load
    hostname = platform.node() if hostname is None else hostname
    notes = []
    model, model_reason, route = select_model(config, usage_directory, now)
    model_prefix = (
        f"model {model['id']} ({model_reason})" if model
        else "no coding model (" + model_reason + ")"
    )

    if mac_only:
        for candidate in config["candidates"]:
            if not candidate.get("mac_only"):
                continue
            if not candidate.get("ready", True):
                note = str(candidate.get("note") or "").strip()
                notes.append(f"{candidate['id']} skipped: not ready" + (f": {note}" if note else ""))
                continue
            load = None
            if _is_local_candidate(candidate, hostname):
                try:
                    load = local_loader()
                except Exception:  # local measurement must not break selection
                    load = None
            else:
                load = read_report(candidate["id"], directory)
            host_reason = "Mac-only task: " + "; ".join(notes + [f"{candidate['id']} is the Mac-only host"])
            return _result(candidate, load, True, model_prefix + "; " + host_reason,
                           config, model, model_reason, route)
        host_reason = "; ".join(notes) or "no mac-only candidate is available"
        return _result(None, None, True, model_prefix + "; " + host_reason,
                       config, model, model_reason, route)

    for candidate in config["candidates"]:
        if candidate.get("mac_only"):
            continue
        load, why = _candidate_load(candidate, config, directory, local_loader, now, hostname)
        if load is not None:
            host_reason = "; ".join(notes + [f"{candidate['id']} is ready, fresh and under thresholds"])
            return _result(candidate, load, False, model_prefix + "; " + host_reason,
                           config, model, model_reason, route)
        notes.append(f"{candidate['id']} skipped: {why}")

    notes.append("mac-mini is never used for non-mac work")
    return _result(None, None, False, model_prefix + "; " + "; ".join(notes),
                   config, model, model_reason, route)


def format_human(result):
    model = result.get("model") or "none"
    effort = f", effort {result['model_effort']}" if result.get("model_effort") else ""
    tool = f", {result['model_tool']}" if result.get("model_tool") else ""
    if not result.get("host"):
        return f"host: none; model: {model}{effort}{tool}; {result.get('reason') or 'no candidate available'}"
    tailscale = result.get("tailscale") or result["host"]
    return (f"host: {result['host']} ({tailscale}); "
            f"model: {model}{effort}{tool}; {result['reason']}")


def live_endpoint(url):
    """Map a kiwi-live base or /mcp URL to the /host-load endpoint."""
    value = str(url or "").strip().rstrip("/")
    if value.endswith("/mcp"):
        value = value[:-4]
    if value.endswith("/host-load"):
        return value
    return value + "/host-load"


def report_host(host_id, root=None, reports=None, url=None, token=None, opener=None, now=None):
    """Measure this machine, store the report, then POST it to kiwi-live."""
    config = load_config(root)
    known = {str(candidate.get("id")) for candidate in config["candidates"]}
    if str(host_id) not in known:
        raise ValueError(f"unknown host {host_id!r}; add it to tokens/tokens.json dsh_host.candidates")
    record = measure_local_load(now)
    record["host"] = str(host_id)
    path = write_report(host_id, record, reports)
    url = url or os.environ.get("KIWI_LIVE_URL")
    token = token or os.environ.get("KIWI_LIVE_TOKEN")
    if not url:
        raise RuntimeError("KIWI_LIVE_URL is required (for example https://kiwi-host.example.net/mcp)")
    if not token:
        raise RuntimeError("KIWI_LIVE_TOKEN is required")
    body = json.dumps(record, ensure_ascii=False).encode("utf-8")
    request = urllib.request.Request(
        live_endpoint(url), data=body, method="POST",
        headers={"Content-Type": "application/json", "Authorization": "Bearer " + token},
    )
    response = opener or urllib.request.urlopen
    with response(request, timeout=15) as reply:
        payload = reply.read().decode("utf-8", "replace")
    return {"host": str(host_id), "stored": str(path), "status": getattr(reply, "status", None), "response": payload[:400]}


def main(argv=None):
    parser = argparse.ArgumentParser(description="Choose or report the dsh host from tokens/tokens.json dsh_host")
    subparsers = parser.add_subparsers(dest="command", required=True)

    select = subparsers.add_parser("select", help="print the host to use now")
    select.add_argument("--mac-only", action="store_true", help="choose the Mac mini for a Mac-only task")
    select.add_argument("--json", action="store_true", help="print the full result as JSON")
    select.add_argument("--root", help="Kiwi checkout (default: KIWI_ROOT or this checkout)")
    select.add_argument("--reports-dir", help="host-load report directory (default: ~/.kiwi/host-load)")
    select.add_argument("--usage-dir", help="coding-model usage report directory (default: dsh_host.coding_route.usage_dir)")

    report = subparsers.add_parser("report", help="measure and push this machine's load")
    report.add_argument("--host", required=True, help="candidate id from tokens/tokens.json")
    report.add_argument("--root", help="Kiwi checkout (default: KIWI_ROOT or this checkout)")
    report.add_argument("--reports-dir", help="host-load report directory (default: ~/.kiwi/host-load)")
    report.add_argument("--url", help="kiwi-live URL (default: KIWI_LIVE_URL)")
    report.add_argument("--token", help="kiwi-live bearer token (default: KIWI_LIVE_TOKEN)")

    args = parser.parse_args(argv)
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, ValueError):
        pass
    try:
        if args.command == "select":
            result = select_host(args.mac_only, root=args.root, reports=args.reports_dir,
                                 usage=args.usage_dir)
            print(json.dumps(result, ensure_ascii=False, indent=2) if args.json else format_human(result))
            return 0 if result["host"] and result.get("model") else 1
        result = report_host(args.host, root=args.root, reports=args.reports_dir, url=args.url, token=args.token)
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return 0
    except (OSError, ValueError, RuntimeError) as error:
        print(f"kiwi_dsh_host: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
