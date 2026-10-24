#!/bin/sh
# kiwi-sprouts bootstrap: install missing local tooling, build kiwi-graph,
# link the CLI, then run the unit suite. Idempotent; safe to rerun after a box update.
#
# docs_search never downloads an embedding model here. It reuses the Mac mini
# brain-search/EmbeddingGemma index; the box has no index and falls back to rg.
set -eu

DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
LOCAL="$HOME/.local"
BIN="$LOCAL/bin"
TS_VERSION="${KIWI_SPROUTS_TS_VERSION:-5.9.3}"
GRAPH_TARGET=/tmp/kiwi-graph-target
GRAPH_BIN="$GRAPH_TARGET/release/kiwi-graph"

# Put the local bin, cargo, and a box Swift toolchain on PATH before probing.
PATH="$BIN:$HOME/.cargo/bin:$PATH"
for d in /workspace/swift-tc/*/usr/bin; do
    if [ -x "$d/sourcekit-lsp" ]; then PATH="$d:$PATH"; break; fi
done
export PATH
export NPM_CONFIG_PREFIX="$LOCAL"

say() { printf '%s\n' "bootstrap: $*"; }
warn() { printf '%s\n' "bootstrap: WARN: $*" >&2; }
die() { printf '%s\n' "bootstrap: ERROR: $*" >&2; exit 1; }

# --- python3 >= 3.10 -------------------------------------------------------
command -v python3 >/dev/null 2>&1 || die "python3 >= 3.10 is required"
python3 -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 10) else 1)' \
    || die "python3 >= 3.10 is required (found $(python3 -V 2>&1))"
say "python3: $(python3 -V 2>&1)"

# --- npm tools in ~/.local, only when missing ------------------------------
command -v npm >/dev/null 2>&1 || die "npm is required (to install pyright/typescript-language-server)"
mkdir -p "$BIN" "$LOCAL/lib"

install_npm() {
    say "npm install -g $*"
    npm install -g --prefix "$LOCAL" "$@"
}

command -v pyright-langserver >/dev/null 2>&1 || install_npm pyright
command -v typescript-language-server >/dev/null 2>&1 || install_npm typescript-language-server
command -v tsc >/dev/null 2>&1 || install_npm "typescript@$TS_VERSION"

# TypeScript 7 (the native preview) ships no lib/tsserver.js. Keep a compatible
# 5.x copy aside for typescript-language-server; kiwi_sprouts.py auto-detects it.
TS_GLOBAL="$LOCAL/lib/node_modules/typescript/lib/tsserver.js"
TS_SIDE="$LOCAL/lib/kiwi-sprouts/typescript/node_modules/typescript/lib/tsserver.js"
if [ ! -f "$TS_GLOBAL" ] && [ ! -f "$TS_SIDE" ]; then
    say "installing compatible typescript@$TS_VERSION for tsserver"
    npm install --prefix "$LOCAL/lib/kiwi-sprouts/typescript" "typescript@$TS_VERSION"
fi
if [ ! -f "$TS_GLOBAL" ] && [ ! -f "$TS_SIDE" ]; then
    die "no usable typescript tsserver.js found"
fi

# --- rust-analyzer via rustup, only when missing ---------------------------
if ! rust-analyzer --version >/dev/null 2>&1; then
    command -v rustup >/dev/null 2>&1 \
        || die "rust-analyzer missing and rustup not found (install Rust, or put rust-analyzer on PATH)"
    say "rustup component add rust-analyzer"
    rustup component add rust-analyzer
fi
rust-analyzer --version >/dev/null 2>&1 \
    || die "rust-analyzer is still unavailable after rustup component add"
say "rust-analyzer: $(rust-analyzer --version 2>&1)"

# --- sourcekit-lsp is optional; warn only ----------------------------------
if command -v sourcekit-lsp >/dev/null 2>&1; then
    say "sourcekit-lsp: $(command -v sourcekit-lsp)"
else
    warn "sourcekit-lsp not found; Swift tests will skip (install the Swift toolchain separately)"
fi

# --- build kiwi-graph release only when the binary is missing --------------
if [ ! -x "$GRAPH_BIN" ]; then
    command -v cargo >/dev/null 2>&1 || die "cargo is required to build kiwi-graph"
    [ -f "$DIR/../kiwi-graph/Cargo.toml" ] || die "missing $DIR/../kiwi-graph/Cargo.toml"
    say "cargo build --release -j1 (kiwi-graph)"
    CARGO_TARGET_DIR="$GRAPH_TARGET" cargo build --release -j1 \
        --manifest-path "$DIR/../kiwi-graph/Cargo.toml"
fi
[ -x "$GRAPH_BIN" ] || die "kiwi-graph binary was not built at $GRAPH_BIN"
say "kiwi-graph: $GRAPH_BIN"

# --- link the CLI and run the health check ---------------------------------
[ -f "$DIR/kiwi_sprouts.py" ] || die "missing $DIR/kiwi_sprouts.py"
chmod +x "$DIR/kiwi_sprouts.py"
ln -sfn "$DIR/kiwi_sprouts.py" "$BIN/kiwi-sprouts"
say "linked $BIN/kiwi-sprouts"

if [ "${KIWI_SPROUTS_BUILD_RUST:-0}" = 1 ]; then
    command -v cargo >/dev/null 2>&1 || die "cargo is required to build kiwi-sprouts Rust"
    SPROUTS_TARGET="${CARGO_TARGET_DIR:-$HOME/Scratch/$(date +%F)-kiwi-sprouts-rust/target}"
    say "cargo build --locked --release --bins (kiwi-sprouts Rust)"
    CARGO_TARGET_DIR="$SPROUTS_TARGET" cargo build --locked --release --bins --manifest-path "$DIR/Cargo.toml"
    install -m 755 "$SPROUTS_TARGET/release/kiwi-sprouts" "$BIN/kiwi-sprouts-rs"
    install -m 755 "$SPROUTS_TARGET/release/kiwi-sprouts-test-driver" "$BIN/kiwi-sprouts-test-driver"
    install -m 755 "$SPROUTS_TARGET/release/kiwi-sprouts-fixture" "$BIN/kiwi-sprouts-fixture"
    say "installed $BIN/kiwi-sprouts-rs (MCP configuration still uses Python)"
fi

say "health check: --health runs unittest (no model download)"
"$BIN/kiwi-sprouts" --health
say "ok"
