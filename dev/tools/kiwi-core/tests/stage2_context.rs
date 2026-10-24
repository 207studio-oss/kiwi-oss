mod common;
use common::Env;
use std::path::PathBuf;

fn repo(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let d = std::env::temp_dir().join(format!("kiwi-src-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    for (p, c) in files { let f = d.join(p); std::fs::create_dir_all(f.parent().unwrap()).unwrap(); std::fs::write(f, c).unwrap(); }
    d
}
fn build(k: &Env, src: &PathBuf) -> (i32, serde_json::Value, String) {
    let out = k.home.join("kiwi.db");
    k.run(&["compile", "--src", src.to_str().unwrap(), "--out", out.to_str().unwrap(), "--sha", "abc123"])
}
fn big(n: usize) -> String { "필수 규칙 내용. ".repeat(n) }

#[test]
fn mandatory_over_budget_is_never_truncated_optional_shrinks() {
    let k = Env::new("s2a");
    let m = format!("# Must\n{}", big(800));
    let src = repo("s2a", &[("kiwi.toml", "[compile]\nmandatory=[\"must\"]\n"), ("rules/must.md", &m),
        ("rules/opt-a.md", &format!("# Deploy notes\ndeploy deploy\n{}", "x".repeat(3000))), ("rules/opt-b.md", "# Deploy small\ndeploy tip")]);
    assert_eq!(build(&k, &src).0, 0);
    let (c, b, _) = k.run(&["brief", "--principal", "bot", "--task", "deploy the app", "--budget", "1000"]);
    assert_eq!(c, 0);
    assert_eq!(b["mandatory_rules"][0]["body"].as_str().unwrap(), m.trim());
    assert!(b["bytes"]["mandatory"].as_u64().unwrap() > 1000);
    assert!(b["bytes"]["optional"].as_u64().unwrap() <= 1000);
    assert!(b["selected_context"].as_array().unwrap().iter().any(|s| s["id"] == "opt-b"));
    let a = b["selected_context"].as_array().unwrap().iter().find(|s| s["id"] == "opt-a").unwrap();
    assert_eq!(a["form"], "summary");
}

#[test]
fn packer_is_deterministic_and_reports_omitted_refs() {
    let k = Env::new("s2b");
    let mut files: Vec<(String, String)> = vec![];
    for i in 0..30 { files.push((format!("rules/r{:02}.md", i), format!("# Cache rule {}\ncache {}", i, "y".repeat(400)))); }
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let src = repo("s2b", &refs); build(&k, &src);
    let one = k.run(&["brief", "--principal", "p", "--task", "cache invalidation", "--budget", "600", "--no-cache"]).1;
    let two = k.run(&["brief", "--principal", "p", "--task", "cache invalidation", "--budget", "600", "--no-cache"]).1;
    assert_eq!(one["selected_context"], two["selected_context"]);
    assert_eq!(one["completeness"], "partial");
    assert!(one["omitted_refs"][0]["uri"].as_str().unwrap().starts_with("kiwi://rule/"));
}

#[test]
fn authority_beats_priority_and_unresolved_conflict_blocks_only_that_action() {
    let k = Env::new("s2c");
    let src = repo("s2c", &[
        ("rules/user-says.md", "---\nauthority: user\nmandatory: true\nconflicts: [\"bot-says\"]\npriority: 1\n---\n# U\nuse model A"),
        ("rules/bot-says.md", "---\nauthority: bot\nmandatory: true\npriority: 100\n---\n# B\nuse model B"),
        ("rules/m1.md", "---\nmandatory: true\nactions: [\"merge\"]\nconflicts: [\"m2\"]\n---\n# M1\nsquash"),
        ("rules/m2.md", "---\nmandatory: true\nactions: [\"merge\"]\n---\n# M2\nrebase")]);
    assert_eq!(build(&k, &src).0, 0);
    let b = k.run(&["brief", "--principal", "p", "--task", "x", "--actions", "merge"]).1;
    let ids: Vec<&str> = b["mandatory_rules"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"user-says") && !ids.contains(&"bot-says"));
    assert_eq!(b["blocked_actions"][0]["blocks_actions"][0], "merge");
    assert!(ids.contains(&"m1") && ids.contains(&"m2"));
}

#[test]
fn compile_rejects_broken_links_duplicates_and_missing_mandatory() {
    let k = Env::new("s2d");
    assert_eq!(build(&k, &repo("s2d1", &[("rules/a.md", "# A\nsee [b](b.md)")])).0, 2);
    assert_eq!(build(&k, &repo("s2d2", &[("kiwi.toml", "[compile]\nmandatory=[\"zzz\"]\n"), ("rules/a.md", "# A")])).0, 2);
    let (c, v, _) = build(&k, &repo("s2d3", &[("rules/a.md", "# A\nsame"), ("rules/b.md", "# A\nsame")]));
    assert_eq!(c, 0); assert_eq!(v["duplicate_candidates"], 1);
    assert_eq!(v["nodes"], 2); // relation only, never merged
}

#[test]
fn accounts_are_not_rule_sources() {
    // kiwi.db compiles rules/, bots/, skills only: accounts/ is read through search, not compiled as rules.
    let k = Env::new("s2e");
    let src = repo("s2e", &[("rules/a.md", "# A\nopenai key location"), ("accounts/openai-api.md", "# key\nsk-SECRET"), ("bots/skills/x/SKILL.md", "# X\nok")]);
    build(&k, &src);
    let b = k.run(&["brief", "--principal", "p", "--task", "openai key SECRET", "--no-cache"]).2;
    let out = k.run(&["brief", "--principal", "p", "--task", "openai key SECRET", "--no-cache"]).1.to_string();
    assert!(!out.contains("sk-SECRET"), "{}", b);
}

#[test]
fn supersedes_hides_old_rule() {
    let k = Env::new("s2f");
    build(&k, &repo("s2f", &[("rules/new.md", "---\nmandatory: true\nsupersedes: [\"old\"]\n---\n# N\nnew"), ("rules/old.md", "---\nmandatory: true\n---\n# O\nold")]));
    let ids: Vec<String> = k.run(&["brief", "--principal", "p", "--task", "x"]).1["mandatory_rules"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap().to_string()).collect();
    assert_eq!(ids, vec!["new"]);
}

#[test]
fn new_user_instruction_invalidates_cached_bundle_and_appears_first() {
    let k = Env::new("s2g");
    build(&k, &repo("s2g", &[("rules/a.md", "---\nmandatory: true\n---\n# A\nrule")]));
    assert_eq!(k.run(&["brief", "--principal", "p", "--task", "t"]).1["cache"], "miss");
    assert_eq!(k.run(&["brief", "--principal", "p", "--task", "t"]).1["cache"], "hit");
    k.run(&["intent", "add", "--principal", "user", "--message-ref", "c", "--goal", "Opus only for planning"]);
    let b = k.run(&["brief", "--principal", "p", "--task", "t"]).1;
    assert_eq!(b["cache"], "miss");
    assert_eq!(b["user_intents"][0]["goal"], "Opus only for planning");
    assert_eq!(b["policy_epoch"], 1);
}

#[test]
fn owner_file_is_mandatory_only_for_its_principal() {
    let k = Env::new("s2h");
    build(&k, &repo("s2h", &[("bots/00000000-0000-4000-8000-b379ab5a9208.md", "# Bot\nmy role")]));
    assert_eq!(k.run(&["brief", "--principal", "00000000-0000-4000-8000-b379ab5a9208", "--task", "t"]).1["mandatory_rules"].as_array().unwrap().len(), 1);
    assert_eq!(k.run(&["brief", "--principal", "other", "--task", "t"]).1["mandatory_rules"].as_array().unwrap().len(), 0);
}

fn answer_output(k: &Env, asker: &std::path::Path, command: &str, extra: &[&str], timeout: &str) -> Vec<u8> {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_kiwi"))
        .args([command, "--principal", "bot", "--task", "login workflow"])
        .args(extra).env("KIWI_HOME", &k.home).env_remove("KIWI_DB").env_remove("KIWI_CONTEXT_ANSWER")
        .env("KIWI_ASK", asker).env("KIWI_ASK_TIMEOUT", timeout)
        .env("KIWI_AUTOREFRESH", "off").env("KIWI_SEMANTIC", "off").env("KIWI_DECISIONS", "off")
        .output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    output.stdout
}

fn answer_bundle(k: &Env, asker: &std::path::Path, command: &str, extra: &[&str]) -> serde_json::Value {
    serde_json::from_slice(&answer_output(k, asker, command, extra, "90")).unwrap()
}

fn asker_script(k: &Env, unix: &str, windows: &str) -> PathBuf {
    let path = k.home.join(if cfg!(windows) { "fake-asker.cmd" } else { "fake-asker" });
    std::fs::write(&path, if cfg!(windows) { format!("@echo off\r\n{windows}\r\n") } else { format!("#!/bin/sh\n{unix}\n") }).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

fn without_answer(mut bundle: serde_json::Value) -> serde_json::Value {
    if let Some(object) = bundle.as_object_mut() {
        for key in ["answer", "confidence", "evidence", "needs_user", "ask_id", "answer_detail"] { object.remove(key); }
    }
    bundle
}

#[test]
fn missing_asker_falls_back_to_the_plain_bundle() {
    let k = Env::new("s2-answer-unavailable");
    assert_eq!(build(&k, &repo("s2-answer-unavailable", &[("rules/a.md", "# Login\nlogin workflow")])).0, 0);
    let missing = k.home.join("missing-asker");
    answer_output(&k, &missing, "brief", &[], "90");
    let plain_bytes = answer_output(&k, &missing, "brief", &[], "90");
    let plain: serde_json::Value = serde_json::from_slice(&plain_bytes).unwrap();
    assert!(plain["answer"].is_null());
    assert_eq!(plain["confidence"], 0.0);
    assert_eq!(plain["needs_user"]["reason"], "ask_unavailable");
    assert!(plain["ask_id"].is_null());
    assert_eq!(plain["answer_detail"]["status"], "needs_user");
    assert_eq!(answer_output(&k, &missing, "context_for", &["--options", "bot,user"], "90"), plain_bytes);
    let answered = answer_bundle(&k, &missing, "context_for", &["--question", "who handles login"]);
    assert!(answered["answer"].is_null());
    assert_eq!(answered["needs_user"]["reason"], "ask_unavailable");
    assert_eq!(answered["answer_detail"]["reason"], "ask_unavailable");
    assert!(!answered["answer_detail"]["error"].as_str().unwrap().is_empty());
    assert_eq!(without_answer(answered), without_answer(plain));
    assert_eq!(answer_output(&k, &missing, "brief", &[], "90"), plain_bytes);
}

#[test]
fn answer_is_flat_fresh_on_cache_hits_and_defaults_question_to_task() {
    let k = Env::new("s2-answer");
    assert_eq!(build(&k, &repo("s2-answer", &[("rules/a.md", "# Login\nlogin workflow")])).0, 0);
    let record = k.home.join("ask-args");
    let first_response = k.home.join("first.json");
    std::fs::write(&first_response, serde_json::json!({"status": "answered", "answer": "first", "confidence": 0.9,
        "ask_id": "ask-1", "evidence": [{"id": "rules/a"}], "decisions": ["dec-1"]}).to_string()).unwrap();
    let script = asker_script(&k,
        &format!("printf '%s\\n' \"$@\" > '{}'\ncat '{}'", record.display(), first_response.display()),
        &format!("echo %*>\"{}\"\r\ntype \"{}\"", record.display(), first_response.display()));
    let first = answer_bundle(&k, &script, "brief", &["--question", "first", "--options", "bot,user"]);
    assert_eq!(first["answer"], "first");
    assert_eq!(first["confidence"], 0.9);
    assert!(first["needs_user"].is_null());
    assert_eq!(first["ask_id"], "ask-1");
    assert_eq!(first["evidence"], serde_json::json!(["rules/a", "dec-1"]));
    assert_eq!(first["answer_detail"]["answer"], "first");
    let args = std::fs::read_to_string(&record).unwrap();
    for arg in ["ask", "--question=first", "--json", "--options=bot,user", "--task=login workflow", "--principal=bot"] {
        assert!(args.contains(arg), "{args}");
    }
    let second_response = k.home.join("second.json");
    std::fs::write(&second_response, serde_json::json!({"status": "answered", "answer": "second", "confidence": 0.8, "ask_id": "ask-2"}).to_string()).unwrap();
    let script = asker_script(&k,
        &format!("printf '%s\\n' \"$@\" > '{}'\ncat '{}'", record.display(), second_response.display()),
        &format!("echo %*>\"{}\"\r\ntype \"{}\"", record.display(), second_response.display()));
    let second = answer_bundle(&k, &script, "context_for", &["--question", "second"]);
    assert_eq!(second["cache"], "hit");
    assert_eq!(second["bundle_id"], first["bundle_id"]);
    assert_eq!(second["answer"], "second");
    assert_eq!(second["ask_id"], "ask-2");
    assert!(!std::fs::read_to_string(&record).unwrap().contains("--options"));
    let third = answer_bundle(&k, &script, "brief", &[]);
    assert_eq!(third["answer"], "second");
    assert!(std::fs::read_to_string(record).unwrap().contains("--question=login workflow"));
}

#[test]
fn low_confidence_answer_needs_user_and_keeps_best_guess() {
    let k = Env::new("s2-answer-low");
    assert_eq!(build(&k, &repo("s2-answer-low", &[("rules/a.md", "# Login\nlogin workflow")])).0, 0);
    let response = k.home.join("low.json");
    std::fs::write(&response, serde_json::json!({"status": "answered", "answer": "guess", "confidence": 0.4,
        "ask_id": "ask-low", "reason": "low_confidence", "best_guess": "guess"}).to_string()).unwrap();
    let script = asker_script(&k, &format!("cat '{}'", response.display()), &format!("type \"{}\"", response.display()));
    let bundle = answer_bundle(&k, &script, "brief", &[]);
    assert_eq!(bundle["answer"], "guess");
    assert_eq!(bundle["confidence"], 0.4);
    assert_eq!(bundle["needs_user"]["reason"], "low_confidence");
    assert_eq!(bundle["needs_user"]["best_guess"], "guess");
    assert_eq!(bundle["ask_id"], "ask-low");
    assert_eq!(bundle["selected_context"][0]["id"], "a");
}

#[test]
fn disabled_context_answer_keeps_bundle_without_asking() {
    let k = Env::new("s2-answer-disabled");
    assert_eq!(build(&k, &repo("s2-answer-disabled", &[("rules/a.md", "# Login\nlogin workflow")])).0, 0);
    let bundle = k.run(&["brief", "--principal", "bot", "--task", "login workflow"]).1;
    assert!(bundle["answer"].is_null());
    assert_eq!(bundle["needs_user"]["reason"], "disabled");
    assert!(bundle["answer_detail"].is_null());
    assert_eq!(bundle["selected_context"][0]["id"], "a");
}

#[test]
fn failed_malformed_and_timed_out_asker_preserves_bundle() {
    let k = Env::new("s2-answer-errors");
    assert_eq!(build(&k, &repo("s2-answer-errors", &[("rules/a.md", "# Login\nlogin workflow")])).0, 0);
    for (unix, windows) in [("exit 1", "exit /b 1"), ("echo invalid", "echo invalid"), ("echo null", "echo null"), ("echo '[]'", "echo []")] {
        let script = asker_script(&k, unix, windows);
        let bundle = answer_bundle(&k, &script, "brief", &["--question", "q"]);
        assert_eq!(bundle["needs_user"]["reason"], "error");
        assert!(bundle["answer"].is_null());
        assert_eq!(bundle["answer_detail"]["status"], "needs_user");
        assert!(!bundle["answer_detail"]["error"].as_str().unwrap().is_empty());
        assert_eq!(bundle["selected_context"][0]["id"], "a");
    }
    let script = asker_script(&k, "sleep 2", "powershell.exe -NoProfile -NonInteractive -Command Start-Sleep -Seconds 2");
    let start = std::time::Instant::now();
    let bundle: serde_json::Value = serde_json::from_slice(&answer_output(&k, &script, "brief", &["--question", "q"], "0")).unwrap();
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
    assert_eq!(bundle["needs_user"]["reason"], "error");
    assert!(bundle["answer"].is_null());
    assert!(bundle["answer_detail"]["error"].as_str().unwrap().contains("timed out"));
    assert_eq!(bundle["selected_context"][0]["id"], "a");
}

#[test]
fn large_ask_answer_does_not_block_on_stdout_pipe() {
    let k = Env::new("s2-answer-large");
    assert_eq!(build(&k, &repo("s2-answer-large", &[("rules/a.md", "# Login\nlogin workflow")])).0, 0);
    let expected = serde_json::json!({"status": "answered", "answer": "x".repeat(131072)});
    let response = k.home.join("response.json");
    std::fs::write(&response, expected.to_string()).unwrap();
    let script = asker_script(&k, &format!("cat '{}'", response.display()), &format!("type \"{}\"", response.display()));
    let bundle: serde_json::Value = serde_json::from_slice(&answer_output(&k, &script, "brief", &["--question", "q"], "5")).unwrap();
    assert_eq!(bundle["answer"], expected["answer"]);
    assert_eq!(bundle["answer_detail"], expected);
}
