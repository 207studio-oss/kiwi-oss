import argparse
from datetime import date
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--core-bin", type=Path, default=os.environ.get("KIWI_TEST_CORE_BIN"))
    parser.add_argument("--synthetic-lsp", action="store_true")
    args = parser.parse_args()
    scratch=Path.home()/"Scratch"/(date.today().isoformat()+"-kiwi-sprouts-tests")
    scratch.mkdir(parents=True,exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="sprouts-suite-",dir=scratch) as directory:
        root = Path(directory)
        core=args.core_bin
        if core is None:
            target=root/"core-target"
            result=subprocess.run(["cargo","build","--locked","--manifest-path",str(HERE.parent.parent/"kiwi-core/Cargo.toml"),"--bin","kiwi","--target-dir",str(target)],capture_output=True)
            if result.returncode:
                sys.stderr.buffer.write(result.stdout+result.stderr)
                return result.returncode
            core=target/"debug"/("kiwi.exe" if os.name=="nt" else "kiwi")
        if not core.is_file():
            raise FileNotFoundError(core)
        env = dict(os.environ, KIWI_TEST_CORE_BIN=str(core.resolve()), PYTHONUTF8="1", PYTHONIOENCODING="utf-8",TEMP=str(root),TMP=str(root))
        if args.synthetic_lsp:
            suffix = ".exe" if os.name == "nt" else ""
            fixture = args.binary.resolve().parent / ("kiwi-sprouts-fixture" + suffix)
            for name in ["pyright-langserver", "typescript-language-server", "rust-analyzer", "sourcekit-lsp"]:
                os.link(fixture, root / (name + suffix))
            config = root / "data.json"
            config.write_text(json.dumps({"suite": True}), encoding="utf-8")
            env.update(PATH=str(root) + os.pathsep + env.get("PATH", ""), SPROUTS_FIXTURE_DATA=str(config))
        for backend in ["python", "rust"]:
            current = dict(env)
            current.pop("KIWI_SPROUTS_BIN", None)
            if backend == "rust":
                current["KIWI_SPROUTS_BIN"] = str(args.binary.resolve())
            print(f"EXISTING SUITE backend={backend} synthetic_lsp={args.synthetic_lsp}", flush=True)
            result = subprocess.run([sys.executable, "-m", "unittest", "-v", "test_context_for", "test_deny", "test_sprouts"],
                                    cwd=HERE, env=current, check=False)
            if result.returncode:
                return result.returncode
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
