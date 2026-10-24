import json
import sys
from pathlib import Path


def ask_arguments(method, args):
    allowed = {"ask": ("question", "options", "task", "principal"),
               "ask_resolve": ("ask_id", "actual"), "ask_stats": ("days",)}[method]
    for key in args:
        if key not in allowed:
            raise TypeError(f"Sprouts.{method}() got an unexpected keyword argument '{key}'")
    return {key: args[key] for key in allowed if key in args}


for line in sys.stdin.buffer:
    request = json.loads(line)
    module_path = str(Path(request["root"]) / "tools" / "decisions")
    if module_path not in sys.path:
        sys.path.insert(0, module_path)
    try:
        if request["method"] == "brief":
            import adapter
            result = adapter.brief(request["args"])
        elif request["method"] in ("ask", "ask_resolve", "ask_stats"):
            import ask
            method = getattr(ask, {"ask": "ask", "ask_resolve": "resolve", "ask_stats": "stats"}[request["method"]])
            result = method(**ask_arguments(request["method"], request["args"]))
        else:
            import decide
            result = decide.decide(*request["args"])
        reply = {"result": result}
    except SystemExit as error:
        if request["method"] == "brief":
            sys.stdout.buffer.write(b'{"fatal":true}\n')
            sys.stdout.buffer.flush()
            raise
        reply = {"error": f"Decisions not configured on this machine ({error}). Do NOT ask the user for a key in chat; report 'Decisions 미설정' and stop. Setup: Kiwi tools/windows/kiwi-v2-apply.ps1 / tools/mac/kiwi-v2-apply.sh."}
    except Exception as error:
        reply = {"error": str(error)}
    sys.stdout.buffer.write(json.dumps(reply).encode("ascii") + b"\n")
    sys.stdout.buffer.flush()
