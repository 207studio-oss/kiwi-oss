//! kiwi lint / kiwi index: a clean fixture passes; each violation fails with its own error code.
mod common;
use common::Env;
use std::path::{Path, PathBuf};

const ID: &str = "00000000-0000-4000-8000-cf4c4732fd3b";

fn rule(id: &str, body: &str) -> String {
    format!("---\nid: {id}\ntitle: \"T {id}\"\ndate: 2026-10-23\nsource: \"test\"\nstatus: active\nsummary: \"S {id}\"\n---\n# T {id}\n\n{body}\n")
}

fn fixture(name: &str) -> (Env, PathBuf) {
    let env = Env::new(name);
    let src = env.home.join("repo");
    for d in ["rules", "bots/active"] { std::fs::create_dir_all(src.join(d)).unwrap(); }
    std::fs::write(src.join("rules/a.md"), rule("a", "see [b](b.md)")).unwrap();
    std::fs::write(src.join("rules/b.md"), rule("b", &format!("owner `{ID}`"))).unwrap();
    std::fs::write(src.join("rules/README.md"), "# 공통 규칙\n").unwrap();
    std::fs::write(src.join("bots/registry.toml"), format!("[[bot]]\nid = \"{ID}\"\nname = \"t\"\n")).unwrap();
    std::fs::write(src.join("bots/active/r--x.toml"), format!("repo = \"o/r\"\nagent_id = \"{ID}\"\nstarted = \"2026-10-23T09:00:00+09:00\"\nexpires = \"2099-01-01T00:00:00+09:00\"\n")).unwrap();
    let (c, _, e) = env.run(&["index", "--src", src.to_str().unwrap()]);
    assert_eq!(c, 0, "{e}");
    (env, src)
}

fn lint(env: &Env, src: &Path, extra: &[&str]) -> (i32, String) {
    let mut a = vec!["lint", "--src", src.to_str().unwrap()]; a.extend_from_slice(extra);
    let (c, v, e) = env.run(&a);
    (c, format!("{}{}", v, e))
}

fn expect(name: &str, mutate: impl Fn(&Path), code: &str) {
    let (env, src) = fixture(name);
    mutate(&src);
    let (c, out) = lint(&env, &src, &["--changed", "bots/active/r--x.toml,bots/active/new.toml"]);
    assert_eq!(c, 1, "{name}: expected failure, got {out}");
    assert!(out.contains(code), "{name}: missing {code} in {out}");
}

#[test]
fn clean_fixture_passes() {
    let (env, src) = fixture("lint-clean");
    let (c, out) = lint(&env, &src, &[]);
    assert_eq!(c, 0, "{out}");
    let (c, v, _) = env.run(&["index", "--src", src.to_str().unwrap(), "--check"]);
    assert_eq!((c, v["index"].as_str()), (0, Some("fresh")));
}

#[test]
fn missing_frontmatter_key() { expect("lint-fm", |s| { let t = std::fs::read_to_string(s.join("rules/a.md")).unwrap().replace("source: \"test\"\n", ""); std::fs::write(s.join("rules/a.md"), t).unwrap(); }, "rule_missing_source:rules/a.md"); }

#[test]
fn id_mismatch() { expect("lint-id", |s| { let t = std::fs::read_to_string(s.join("rules/a.md")).unwrap().replace("id: a\n", "id: z\n"); std::fs::write(s.join("rules/a.md"), t).unwrap(); }, "rule_id_mismatch"); }

#[test]
fn missing_h1() { expect("lint-h1", |s| { let t = std::fs::read_to_string(s.join("rules/a.md")).unwrap().replace("# T a\n", ""); std::fs::write(s.join("rules/a.md"), t).unwrap(); }, "rule_missing_h1:rules/a.md"); }

#[test]
fn stale_index() { expect("lint-index", |s| { std::fs::write(s.join("rules/c.md"), rule("c", "x")).unwrap(); }, "rules_index_stale"); }

#[test]
fn broken_link() { expect("lint-link", |s| { std::fs::write(s.join("rules/a.md"), rule("a", "see [gone](gone.md)")).unwrap(); }, "broken_link"); }

#[test]
fn bad_status() { expect("lint-status", |s| { let t = std::fs::read_to_string(s.join("rules/a.md")).unwrap().replace("status: active", "status: maybe"); std::fs::write(s.join("rules/a.md"), t).unwrap(); }, "rule_bad_status"); }

#[test]
fn links_superseded_rule() { expect("lint-sup", |s| { let t = std::fs::read_to_string(s.join("rules/b.md")).unwrap().replace("status: active", "status: superseded"); std::fs::write(s.join("rules/b.md"), t).unwrap(); }, "links_superseded:rules/a.md->rules/b.md"); }

#[test]
fn unregistered_id_in_rule() { expect("lint-uuid", |s| { std::fs::write(s.join("rules/b.md"), rule("b", "owner `00000000-0000-4000-8000-ef8b3302007f`")).unwrap(); }, "unregistered_agent_id:rules/b.md"); }

#[test]
fn new_claim_by_unregistered_bot_is_blocked() {
    expect("lint-claim", |s| { std::fs::write(s.join("bots/active/new.toml"), "repo = \"o/r\"\nagent_id = \"someone\"\nexpires = \"2099-01-01T00:00:00Z\"\n").unwrap(); }, "claim_unregistered_agent_id:bots/active/new.toml");
}

#[test]
fn untouched_or_expired_unregistered_claims_only_warn() {
    let (env, src) = fixture("lint-claim-old");
    std::fs::write(src.join("bots/active/old.toml"), "repo = \"o/r\"\nagent_id = \"someone\"\nexpires = \"2099-01-01T00:00:00Z\"\n").unwrap();
    std::fs::write(src.join("bots/active/gone.toml"), "repo = \"o/r\"\nagent_id = \"someone\"\nexpires = \"2020-01-01T00:00:00Z\"\nstatus = \"expired\"\n").unwrap();
    let (c, out) = lint(&env, &src, &["--changed", "bots/active/gone.toml"]);
    assert_eq!(c, 0, "{out}");
    assert!(out.contains("claim_unregistered_agent_id:bots/active/old.toml"), "{out}");
}

#[test]
fn expired_unmarked_claim_warns() {
    let (env, src) = fixture("lint-exp");
    std::fs::write(src.join("bots/active/r--x.toml"), format!("repo = \"o/r\"\nagent_id = \"{ID}\"\nexpires = \"2020-01-01T00:00:00+09:00\"\n")).unwrap();
    let (c, out) = lint(&env, &src, &[]);
    assert_eq!(c, 0, "{out}");
    assert!(out.contains("claim_expired_unmarked:bots/active/r--x.toml"), "{out}");
}

#[test]
fn bad_claim_expiry() { expect("lint-badexp", |s| { std::fs::write(s.join("bots/active/r--x.toml"), format!("repo = \"o/r\"\nagent_id = \"{ID}\"\nexpires = \"soon\"\n")).unwrap(); }, "claim_bad_expires"); }
