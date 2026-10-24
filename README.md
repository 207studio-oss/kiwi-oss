# Kiwi (OSS)

Kiwi is a rule and context engine for teams of coding agents. An agent asks Kiwi for a brief before it acts, records lessons and decisions through it, and coordinates with other agents through claims, tokens and signed webhooks instead of polling.

This repository is a sanitized subset of a private Kiwi deployment, published automatically. Private data (accounts, agent registry, logs, decisions, projects) is never exported, identifiers are redacted, and a release is pushed only after a fail-closed secret and identifier scan and the test suites pass.

## Contents

| Path | What |
| --- | --- |
| `dev/tools/kiwi-core/` | `kiwi` CLI (Rust): intent and approval ledger, task guard, rule compiler, task briefs (`kiwi brief`), writer transaction, `kiwi lint`, `kiwi oss` |
| `dev/tools/kiwi-sprouts/` | MCP server: code and doc search, `context_for`, and `kiwi_live.py` (HTTP MCP with claims, tokens, signed webhooks and self-improvement loops) |
| `dev/tools/kiwi-agent/` | `kiwi-agent finish`: read-only end-of-task check that writes a receipt |
| `tokens/` | token schema with example values: design, claim and task states, webhook, runtime, self-improvement |
| `rules/`, `lessons/` | a few generic rules and technical lessons in Kiwi's format |

Most in-repo documentation is in Korean.

## Build and test

```sh
cargo test --locked --manifest-path dev/tools/kiwi-core/Cargo.toml
cargo test --locked --manifest-path dev/tools/kiwi-agent/Cargo.toml
cargo test --locked --manifest-path dev/tools/kiwi-sprouts/Cargo.toml
(cd dev/tools/kiwi-sprouts/tests && python3 -m unittest discover -p 'test_*.py')
cargo run --manifest-path dev/tools/kiwi-core/Cargo.toml -- compile --src . --out /tmp/kiwi.db
```

The Python tests need Python 3.12 or newer and git.

## Releases

`kiwi oss` builds each release from one commit of the private repository, configured by its `oss.*` tokens: paths to include and exclude, redaction rules, scan patterns, tests, changelog rules and the release handoff. If the scan finds anything or a test fails, nothing is published. `CHANGELOG.md` lists user-visible changes only.

## License

No license has been chosen yet. Until a LICENSE file is added, default copyright applies: you can read the code, but reuse needs permission.
