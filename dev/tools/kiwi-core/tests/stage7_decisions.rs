mod common;
use common::Env;
use std::process::{Command, Stdio};
use std::io::Write;

fn setup(k: &Env) {
    let src = k.home.join("src"); std::fs::create_dir_all(src.join("rules")).unwrap();
    std::fs::write(src.join("rules/must.md"), format!("---\nmandatory: true\n---\n# M\n{}", "m".repeat(5000))).unwrap();
    for (id, body) in [("alpha", "deploy deploy deploy notes"), ("beta", "deploy checklist"), ("gamma", "deploy misc")] {
        std::fs::write(src.join(format!("rules/{}.md", id)), format!("# {}\n{} {}", id, body, "x".repeat(300))).unwrap();
    }
    k.run(&["compile", "--src", src.to_str().unwrap(), "--out", k.home.join("kiwi.db").to_str().unwrap(), "--sha", "s"]);
}
fn scorer(k: &Env, body: &str) -> std::path::PathBuf {
    let p = k.home.join("scorer"); let cnt = k.home.join("calls");
    std::fs::write(&p, format!("#!/bin/sh\ncat >/dev/null\necho x >> '{}'\n{}\n", cnt.display(), body)).unwrap();
    Command::new("chmod").arg("+x").arg(&p).status().unwrap(); p
}
fn brief(k: &Env, sc: &std::path::Path, budget: &str) -> serde_json::Value {
    let o = Command::new(env!("CARGO_BIN_EXE_kiwi")).args(["brief", "--principal", "p", "--task", "deploy the app", "--budget", budget, "--no-cache"])
        .env("KIWI_HOME", &k.home).env_remove("KIWI_DB").env("KIWI_SCORER", sc).env("KIWI_AUTOREFRESH", "off").env("KIWI_SEMANTIC", "off").env_remove("KIWI_DECISIONS").env("KIWI_CONTEXT_ANSWER", "off").output().unwrap();
    serde_json::from_slice(&o.stdout).unwrap()
}
fn calls(k: &Env) -> usize { std::fs::read_to_string(k.home.join("calls")).map(|s| s.lines().count()).unwrap_or(0) }

#[test]
fn decisions_rank_optional_context_and_cache_by_task_and_versions() {
    let k = Env::new("s7a"); setup(&k);
    let sc = scorer(&k, "echo '{\"gamma\": 1.0, \"beta\": 0.5, \"alpha\": 0.0, \"must\": 1.0}'");
    let b = brief(&k, &sc, "400");
    assert_eq!(b["decisions"], "scored");
    assert_eq!(b["selected_context"][0]["id"], "gamma");
    assert_eq!(b["mandatory_rules"][0]["id"], "must");
    assert!(b["mandatory_rules"][0]["body"].as_str().unwrap().len() > 5000, "mandatory never truncated");
    assert!(b["bytes"]["optional"].as_u64().unwrap() <= 400);
    let b2 = brief(&k, &sc, "400");
    assert_eq!(b2["decisions"], "cache_hit"); assert_eq!(calls(&k), 1);
}

#[test]
fn api_failure_falls_back_to_hybrid_and_keeps_mandatory() {
    let k = Env::new("s7b"); setup(&k);
    let sc = scorer(&k, "exit 1");
    let b = brief(&k, &sc, "4096");
    assert_eq!(b["decisions"], "fallback_api");
    assert_eq!(b["selected_context"][0]["id"], "alpha"); // lexical: most "deploy"
    assert_eq!(b["mandatory_rules"][0]["id"], "must");
}

fn compact(k: &Env, sc: Option<&std::path::Path>, input: &str, budget: &str) -> serde_json::Value {
    let mut c = Command::new(env!("CARGO_BIN_EXE_kiwi"));
    c.args(["compact", "--task", "fix the writer bug", "--budget", budget]).env("KIWI_HOME", &k.home).env_remove("KIWI_DECISIONS").stdin(Stdio::piped()).stdout(Stdio::piped());
    match sc { Some(p) => { c.env("KIWI_SCORER", p); } None => { c.env("KIWI_SCORER", "/nonexistent"); } }
    let mut ch = c.spawn().unwrap(); ch.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    serde_json::from_slice(&ch.wait_with_output().unwrap().stdout).unwrap()
}

#[test]
fn compact_keeps_pinned_and_relevant_chunks_in_order() {
    let k = Env::new("s7c");
    let input = format!("goal: fix writer\n\nc1 {}\n\nc2 writer stack trace {}\n\n[pin] user said: no force push\n\nc4 {}\n\nlast message", "a".repeat(200), "b".repeat(200), "d".repeat(200));
    let sc = scorer(&k, "echo '{\"c1\": 0.0, \"c2\": 1.0, \"c4\": 0.1}'");
    let v = compact(&k, Some(&sc), &input, "600");
    let t = v["text"].as_str().unwrap();
    assert!(t.starts_with("goal: fix writer") && t.ends_with("last message"));
    assert!(t.contains("[pin] user said") && t.contains("writer stack trace") && !t.contains("c1 aaa"));
    assert!(t.find("writer stack").unwrap() < t.find("[pin]").unwrap(), "original order");
    assert_eq!(v["decisions"], "scored");
    assert_eq!(compact(&k, None, &input, "600")["decisions"], "cache_hit"); // same task+chunks: no second API call
    let k2 = Env::new("s7c2");
    let f = compact(&k2, None, &input, "600");
    assert_eq!(f["decisions"], "fallback_no_scorer");
    assert!(f["text"].as_str().unwrap().contains("c4 ddd"), "fallback keeps most recent");
}

#[test]
fn similar_task_wording_hits_decisions_cache() {
    let k = Env::new("s7norm"); setup(&k);
    let sc = scorer(&k, "echo '{\"gamma\": 1.0, \"beta\": 0.5}'");
    let run = |task: &str| -> serde_json::Value {
        let o = Command::new(env!("CARGO_BIN_EXE_kiwi")).args(["brief", "--principal", "p", "--task", task, "--budget", "400"])
            .env("KIWI_HOME", &k.home).env_remove("KIWI_DB").env("KIWI_SCORER", &sc).env("KIWI_AUTOREFRESH", "off").env("KIWI_SEMANTIC", "off").env_remove("KIWI_DECISIONS").output().unwrap();
        serde_json::from_slice(&o.stdout).unwrap()
    };
    assert_eq!(run("Deploy the app!")["cache"], "miss");
    assert_eq!(run("the  app deploy")["cache"], "hit");
    assert_eq!(calls(&k), 1);
    assert_eq!(run("deploy the database")["cache"], "miss");
    assert_eq!(calls(&k), 2);
}
