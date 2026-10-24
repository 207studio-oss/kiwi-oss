import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import kiwi_sprouts as original

TOOLS = original.TOOLS
KIWI = original.KIWI
PYTHON_HOME = Path.home()
HERE = Path(__file__).resolve().parent.parent


def command():
    binary = os.environ.get("KIWI_SPROUTS_BIN")
    return [binary] if binary else [sys.executable, str(HERE / "kiwi_sprouts.py")]


def environment():
    env = dict(os.environ, KIWI_ROOT=str(KIWI))
    if Path.home() != PYTHON_HOME:
        env["USERPROFILE" if os.name == "nt" else "HOME"] = str(Path.home())
    return env


class RustSprouts:
    def __init__(self, root=None):
        self.root = Path(root or os.environ.get("KIWI_SPROUTS_ROOT", os.getcwd())).resolve()
        binary = Path(os.environ["KIWI_SPROUTS_BIN"])
        driver = binary.parent / ("kiwi-sprouts-test-driver.exe" if os.name == "nt" else "kiwi-sprouts-test-driver")
        self.process = subprocess.Popen([str(driver), str(self.root)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.PIPE, text=True, encoding="utf-8", env=environment())

    def _call(self, name, args):
        self.process.stdin.write(json.dumps(dict(name=name, args=args)) + "\n")
        self.process.stdin.flush()
        line = self.process.stdout.readline()
        if not line:
            raise RuntimeError(self.process.stderr.read())
        reply = json.loads(line)
        if "error" in reply:
            error = ValueError if reply["error"] in ("path outside root", "path denied", "denied") else RuntimeError
            raise error(reply["error"])
        return reply["result"]

    def abs(self, path):
        return Path(self._call("abs", {"path": path}))

    def _files(self, path="."):
        return [Path(p) for p in self._call("files", {"path": path})]

    def close(self):
        if self.process.stdin and not self.process.stdin.closed:
            self.process.stdin.close()
        self.process.wait(timeout=20)
        self.process.stdout.close()
        self.process.stderr.close()

    def __del__(self):
        if hasattr(self, "process"):
            self.close()

    def __getattr__(self, name):
        def invoke(*args, **kwargs):
            keys = list(TOOLS[name][1])
            return self._call(name, {**dict(zip(keys, args)), **kwargs})
        return invoke


Sprouts = RustSprouts if os.environ.get("KIWI_SPROUTS_BIN") else original.Sprouts


def call(sp, name, args):
    return sp._call(name, args) if isinstance(sp, RustSprouts) else original.call(sp, name, args)


def denied(path):
    if not os.environ.get("KIWI_SPROUTS_BIN"):
        return original.denied(path)
    sp = RustSprouts()
    try:
        return sp._call("denied", {"path": path})
    finally:
        sp.close()


def mcp(sp):
    if not isinstance(sp, RustSprouts):
        return original.mcp(sp)
    result = subprocess.run([*command(), "--root", str(sp.root), "mcp"], input=sys.stdin.read(), capture_output=True,
                            text=True, encoding="utf-8", env=environment(), timeout=60, check=True)
    sys.stdout.write(result.stdout)


if not os.environ.get("KIWI_SPROUTS_BIN"):
    original.command = command
    sys.modules[__name__] = original
