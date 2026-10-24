mod common;
use common::Env;
use std::process::Command;

#[test]
fn semantic_scores_pull_in_paraphrased_rule_but_never_touch_mandatory() {
    let k = Env::new("s6");
    let src = k.home.join("src"); std::fs::create_dir_all(src.join("rules")).unwrap();
    std::fs::write(src.join("rules/must.md"), "---\nmandatory: true\n---\n# M\nalways").unwrap();
    std::fs::write(src.join("rules/release.md"), "# Release gate\nvibe check before App Store").unwrap();
    std::fs::write(src.join("rules/cats.md"), "# Cats\nfood").unwrap();
    let db = k.home.join("kiwi.db");
    k.run(&["compile", "--src", src.to_str().unwrap(), "--out", db.to_str().unwrap(), "--sha", "s"]);
    rusqlite::Connection::open(&db).unwrap().execute_batch("CREATE TABLE vec(id TEXT PRIMARY KEY, hash TEXT, dim INTEGER, v BLOB)").unwrap();
    let fake = k.home.join("fake-embed");
    std::fs::write(&fake, "#!/bin/sh\necho '{\"release\": 0.9, \"cats\": 0.1, \"must\": 0.0}'\n").unwrap();
    Command::new("chmod").arg("+x").arg(&fake).status().unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_kiwi")).args(["brief", "--principal", "p", "--task", "출시 전에 뭘 봐야 해?"])
        .env("KIWI_HOME", &k.home).env_remove("KIWI_DB").env("KIWI_EMBED", &fake).env("KIWI_AUTOREFRESH", "off").env("KIWI_DECISIONS", "off").env("KIWI_CONTEXT_ANSWER", "off").output().unwrap();
    let b: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(b["semantic"], true);
    assert_eq!(b["selected_context"][0]["id"], "release");
    assert!(b["selected_context"].as_array().unwrap().iter().all(|s| s["id"] != "cats"));
    assert_eq!(b["mandatory_rules"][0]["id"], "must");
}
