// kiwi oss against a fixture repo, a local bare remote and a loopback /events listener. Identifiers and the fake key are
// assembled at runtime so this file itself passes the real export scan.
mod common;
use common::Env;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

const BEARER: &str = "local-bearer";

fn git_at(dir: &Path, args: &[&str], at: Option<i64>) -> String {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).args(args).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@example.test").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@example.test");
    if let Some(t) = at { let d = format!("@{} +0000", t); c.env("GIT_AUTHOR_DATE", &d).env("GIT_COMMITTER_DATE", &d); }
    let o = c.output().unwrap();
    assert!(o.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}
fn git(dir: &Path, args: &[&str]) -> String { git_at(dir, args, None) }
fn put(root: &Path, rel: &str, text: &str) {
    let f = root.join(rel);
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(f, text).unwrap();
}
fn now() -> i64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 }
fn slash(p: &Path) -> String { p.to_string_lossy().replace('\\', "/") }

/// Received events: (authorized, event, data). Answers 202 like kiwi-live /events.
fn listener() -> (String, Arc<Mutex<Vec<(bool, String, Value)>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}/events", l.local_addr().unwrap().port());
    let got = Arc::new(Mutex::new(vec![]));
    let sink = got.clone();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let mut r = BufReader::new(s.try_clone().unwrap());
            let (mut auth, mut len) = (false, 0usize);
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" { break; }
                let low = line.to_ascii_lowercase();
                if low.starts_with("authorization:") { auth = line.trim_end().ends_with(&format!("Bearer {}", BEARER)); }
                if let Some(n) = low.strip_prefix("content-length:") { len = n.trim().parse().unwrap_or(0); }
            }
            let mut body = vec![0; len];
            r.read_exact(&mut body).unwrap();
            let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let mut got = sink.lock().unwrap();
            got.push((auth, v["event"].as_str().unwrap_or("").to_string(), v["data"].clone()));
            let reply = json!({"id": format!("ev{}", got.len())}).to_string();
            let mut s = s;
            let _ = write!(s, "HTTP/1.1 202 Accepted\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", reply.len(), reply);
        }
    });
    (url, got)
}

struct Fx { k: Env, src: PathBuf, remote: PathBuf, token_file: PathBuf, events: Arc<Mutex<Vec<(bool, String, Value)>>>, uuid: String, host: String }

/// Source repo: included tool files (one executable), an excluded accounts/ file, a private rule, an overlay README,
/// and tokens.json whose `oss` section points at the local remote and listener (plus an unlisted private section).
fn setup(name: &str, test_cmd: &[&str]) -> Fx {
    let k = Env::new(name);
    let (src, remote, token_file) = (k.home.join("src"), k.home.join("remote.git"), k.home.join("live-token"));
    git(&k.home, &["init", "-q", "--bare", "-b", "main", &slash(&remote)]);
    git(&k.home, &["init", "-q", "-b", "main", &slash(&src)]);
    std::fs::write(&token_file, BEARER).unwrap();
    let (url, events) = listener();
    let uuid = format!("{}-{}-4{}-a{}-{}", "1b2c3d4e", "5f60", "718", "293", "a4b5c6d7e8f9");
    let host = format!("zz{}{}", "host", 1);
    let oss = json!({
        "enabled": true, "repo": "acme/demo-oss", "web_url": "https://example.test/acme/demo-oss", "remote_url": slash(&remote),
        "branch": "main", "clone_dir": "oss/clone", "overlay_dir": "oss/overlay/", "license": null,
        "git": {"name": "Demo OSS", "email": "demo@example.test"}, "version": {"first": "0.1.0", "tag_prefix": "v"},
        "improved": {"event": "kiwi.improved", "window_minutes": 60}, "released": {"event": "oss.released"}, "events_url": url,
        "include": ["dev/tools/demo/", "rules/", "tokens/tokens.json"], "exclude": ["accounts/", "rules/private.md"],
        "tokens": {"path": "tokens/tokens.json", "keep": ["color", "machine", "owner_id"], "set": {"source": "Example values."}},
        "redact": [{"pattern": "(?i)\\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\\b", "replace": "{hash_uuid}"}, {"pattern": "zzhost[0-9]+", "replace": "host"}],
        "scan": {
            "patterns": [{"kind": "uuid", "pattern": "(?i)\\b([0-9a-f]{8})-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\\b", "keep": ["00000000"]},
                         {"kind": "secret_key", "pattern": "\\bsk-[A-Za-z0-9_-]{16,}"}, {"kind": "machine", "pattern": "zzhost"}],
            "blocklist": ["secretproject"],
        },
        "changelog": {
            "surfaces": [{"name": "tools", "paths": ["dev/tools/demo/"]}], "ignore": ["**/tests/"], "skip": ["^(?i)(chore|test)(\\(.*\\))?:"],
            "strip": ["\\s*\\(#[0-9]+\\)"], "kinds": [{"kind": "fix", "words": ["fix"]}, {"kind": "feature", "words": ["add"]}], "default_kind": "improvement",
            "empty_entry": "Internal maintenance only.", "max_entries": 30,
            "first_release": [{"kind": "feature", "title": "Demo tool", "surface": "tools", "paths": ["dev/tools/demo/"]}],
        },
        "marketing": {"highlights": 3, "demo": [{"surface": "tools", "show": "run the demo"}], "video": {"aspect": "1:1"}, "copy": {"max_chars": 280}, "approval": {"x_post": "owner"}},
        "tests": [{"name": "smoke", "cwd": "", "cmd": test_cmd}],
    });
    let tokens = json!({"color": {"accent": "#00aa00"}, "machine": host, "owner_id": uuid, "lab": {"note": "unlisted"}, "oss": oss});
    put(&src, "tokens/tokens.json", &serde_json::to_string_pretty(&tokens).unwrap());
    put(&src, "dev/tools/demo/main.txt", &format!("runs on {}\nsee [accounts](../../../accounts/a.md) and [rule](../../../rules/a.md)\n", host));
    put(&src, "dev/tools/demo/id.txt", &format!("owner {}\n", uuid));
    put(&src, "dev/tools/demo/run.sh", "#!/bin/sh\necho demo\n");
    put(&src, "accounts/a.md", "private\n");
    put(&src, "rules/a.md", "---\nsummary: Rule a\n---\n# A\n");
    put(&src, "rules/private.md", "---\nsummary: Private rule\n---\n# P\n");
    put(&src, "oss/overlay/README.md", "# demo-oss\n");
    put(&src, "oss/overlay/rules/README.md", "# Rules\n\n<!-- kiwi:rules-index:start (generated by `kiwi index`; edit rule frontmatter, not this block) -->\n<!-- kiwi:rules-index:end -->\n");
    // Executable on disk too, or a later `git add -A` on a filemode filesystem records a mode change.
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(src.join("dev/tools/demo/run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap(); }
    git(&src, &["add", "-A"]);
    git(&src, &["update-index", "--chmod=+x", "dev/tools/demo/run.sh"]);
    git(&src, &["commit", "-qm", "init"]);
    Fx { k, src, remote, token_file, events, uuid, host }
}

impl Fx {
    fn oss(&self, args: &[&str]) -> (i32, Value, String) { self.oss_at(&self.k.home, args) }
    /// `kiwi oss` with another KIWI_HOME: a second machine with no local release state.
    fn oss_at(&self, home: &Path, args: &[&str]) -> (i32, Value, String) {
        let mut c = Command::new(env!("CARGO_BIN_EXE_kiwi"));
        c.arg("oss").args(args).args(["--repo", &slash(&self.src)]).env("KIWI_HOME", home).env("KIWI_LIVE_TOKEN_FILE", &self.token_file);
        let o = c.output().unwrap();
        let out = String::from_utf8_lossy(&o.stdout).to_string();
        (o.status.code().unwrap_or(-1), serde_json::from_str(&out).unwrap_or(Value::Null), out + &String::from_utf8_lossy(&o.stderr))
    }
    fn commit(&self, files: &[(&str, &str)], msg: &str, at: Option<i64>) {
        for (p, t) in files { put(&self.src, p, t); }
        git(&self.src, &["add", "-A"]);
        git_at(&self.src, &["commit", "-qm", msg], at);
    }
    fn remote(&self, args: &[&str]) -> String { git(&self.k.home, &[&["--git-dir", &slash(&self.remote)][..], args].concat()) }
    fn events(&self, name: &str) -> Vec<Value> { self.events.lock().unwrap().iter().filter(|(_, e, _)| e == name).map(|(_, _, d)| d.clone()).collect() }
}

fn tree_text(root: &Path) -> String {
    let mut all = String::new();
    for d in std::fs::read_dir(root).unwrap().flatten() {
        let p = d.path();
        if p.is_dir() { all += &tree_text(&p); } else { all += &format!("{}\n{}\n", p.display(), String::from_utf8_lossy(&std::fs::read(&p).unwrap())); }
    }
    all
}

#[test]
fn export_is_scoped_redacted_and_scanned() {
    let fx = setup("oss1", &["git", "--version"]);
    let out = fx.k.home.join("out");
    let (code, v, raw) = fx.oss(&["export", "--out", &slash(&out)]);
    assert_eq!(code, 0, "{}", raw);
    assert_eq!(v["scan"]["passed"], true, "{}", v);
    assert_eq!((v["excluded"]["accounts/"].as_i64(), v["excluded"]["rules/private.md"].as_i64()), (Some(1), Some(1)), "{}", v);
    assert!(out.join("README.md").is_file() && out.join("dev/tools/demo/run.sh").is_file());
    assert!(!out.join("accounts").exists() && !out.join("rules/private.md").exists() && !out.join("oss").exists());
    let all = tree_text(&out);
    assert!(!all.contains(&fx.uuid) && !all.contains(&fx.host), "identifier left in export");
    let tokens: Value = serde_json::from_str(&std::fs::read_to_string(out.join("tokens/tokens.json")).unwrap()).unwrap();
    assert!(tokens.get("oss").is_none() && tokens["machine"] == "host" && tokens["source"] == "Example values.", "{}", tokens);
    // Only `tokens.keep` sections go out; an unlisted section stays private and is reported.
    assert!(tokens.get("lab").is_none(), "{}", tokens);
    assert_eq!((v["excluded"]["tokens/tokens.json#lab"].as_i64(), v["excluded"]["tokens/tokens.json#oss"].as_i64()), (Some(1), Some(1)), "{}", v);
    // The same id becomes the same placeholder everywhere.
    let fake = tokens["owner_id"].as_str().unwrap().to_string();
    assert!(fake.starts_with("00000000-") && std::fs::read_to_string(out.join("dev/tools/demo/id.txt")).unwrap().contains(&fake));
    // Links to files left out become plain text; links to exported files stay.
    let main = std::fs::read_to_string(out.join("dev/tools/demo/main.txt")).unwrap();
    assert!(main.contains("runs on host") && main.contains("[rule](../../../rules/a.md)"), "{}", main);
    let index = std::fs::read_to_string(out.join("rules/README.md")).unwrap();
    assert!(index.contains("a.md") && !index.contains("private"), "{}", index);
}

#[test]
fn scan_fails_closed_without_the_value_and_announces_the_batch_once() {
    let fx = setup("oss2", &["git", "--version"]);
    let key = format!("sk-{}", "Q".repeat(24));
    fx.commit(&[("dev/tools/demo/leak.txt", &format!("key {}\n", key))], "Add leak", None);
    let (code, v, raw) = fx.oss(&["export", "--out", &slash(&fx.k.home.join("out"))]);
    assert_eq!(code, 1, "{}", raw);
    assert_eq!(v["scan"]["passed"], false);
    assert!(v["scan"]["findings"].as_array().unwrap().iter().any(|f| f["kind"] == "secret_key" && f["path"] == "dev/tools/demo/leak.txt"), "{}", v);
    assert!(!raw.contains(&key), "scan printed the secret");

    let (code, v, raw) = fx.oss(&["flush"]);
    assert_eq!((code, v["reason"].as_str()), (1, Some("scan_failed")), "{}", raw);
    assert!(!raw.contains(&key));
    assert_eq!(fx.remote(&["for-each-ref"]), "", "nothing may reach the remote");
    // A still-blocked batch is not announced again.
    let (code, v, _) = fx.oss(&["flush"]);
    assert_eq!((code, v["reason"].as_str(), v["events"].as_array().map(Vec::len)), (1, Some("scan_failed"), Some(0)), "{}", v);
    let improved = fx.events("kiwi.improved");
    assert_eq!(improved.len(), 1);
    assert_eq!((improved[0]["closed_by"].as_str(), improved[0]["count"].as_i64()), (Some("flush"), Some(2)), "{}", improved[0]);
    assert!(fx.events.lock().unwrap().iter().all(|(auth, _, _)| *auth));
}

#[test]
fn flush_publishes_then_tick_batches_and_filters_the_changelog() {
    let fx = setup("oss3", &["git", "--version"]);
    let (code, v, raw) = fx.oss(&["tick"]);
    assert_eq!((code, v["reason"].as_str(), v["pending"].as_i64()), (0, Some("batching"), Some(1)), "{}", raw);
    assert!(fx.events("kiwi.improved").is_empty());

    let (code, v, raw) = fx.oss(&["flush"]);
    assert_eq!((code, v["published"].as_bool(), v["tag"].as_str()), (0, Some(true), Some("v0.1.0")), "{}", raw);
    assert_eq!(fx.remote(&["rev-parse", "main"]), v["oss_sha"].as_str().unwrap());
    assert_eq!(fx.remote(&["log", "-1", "--format=%an <%ae>|%cn <%ce>", "main"]), "Demo OSS <demo@example.test>|Demo OSS <demo@example.test>");
    assert!(fx.remote(&["log", "-1", "--format=%B", "main"]).contains(&format!("Source-Commit: {}", git(&fx.src, &["rev-parse", "HEAD"]))));
    assert_eq!(fx.remote(&["tag", "-l"]), "v0.1.0");
    let tree = fx.remote(&["ls-tree", "-r", "main"]);
    assert!(tree.lines().any(|l| l.starts_with("100755") && l.ends_with("\tdev/tools/demo/run.sh")), "{}", tree);
    assert!(tree.lines().any(|l| l.starts_with("100644") && l.ends_with("\tdev/tools/demo/main.txt")), "{}", tree);
    assert!(fx.remote(&["show", "main:CHANGELOG.md"]).contains("## v0.1.0"));
    let rel = fx.events("oss.released");
    assert_eq!(rel.len(), 1);
    let p = &rel[0];
    assert_eq!((p["version"].as_str(), p["first_release"].as_bool(), p["previous"].is_null()), (Some("0.1.0"), Some(true), true), "{}", p);
    assert_eq!((p["changes"][0]["title"].as_str(), p["scan"]["passed"].as_bool(), p["approval"]["x_post"].as_str()), (Some("Demo tool"), Some(true), Some("owner")));
    assert_eq!((p["demo"][0]["show"].as_str(), p["tree_url"].as_str()), (Some("run the demo"), Some("https://example.test/acme/demo-oss/tree/v0.1.0")));

    let (_, v, raw) = fx.oss(&["flush"]);
    assert_eq!(v["reason"], "no_changes", "{}", raw);
    // Ids that redact to the same text change nothing public.
    fx.commit(&[("dev/tools/demo/main.txt", &std::fs::read_to_string(fx.src.join("dev/tools/demo/main.txt")).unwrap().replace(&fx.host, &format!("zz{}{}", "host", 2)))], "Move host", None);
    let (_, v, raw) = fx.oss(&["flush"]);
    assert_eq!(v["reason"], "unchanged", "{}", raw);

    // A batch whose oldest change is past the window closes on tick; only the user-visible change reaches the changelog.
    let old = now() - 2 * 86400;
    fx.commit(&[("dev/tools/demo/main.txt", "tidy\n")], "chore: tidy", Some(old));
    fx.commit(&[("dev/tools/demo/widget.txt", "widget\n")], "Add widget command (#12)", Some(old + 1));
    fx.commit(&[("dev/tools/demo/parse.txt", "parser\n")], "fix: secretproject parser", Some(old + 2));
    fx.commit(&[("dev/tools/demo/tests/t.txt", "t\n")], "Add tests", Some(old + 3));
    fx.commit(&[("accounts/b.md", "private\n")], "Add account", Some(old + 4));
    let (code, v, raw) = fx.oss(&["tick"]);
    assert_eq!((code, v["tag"].as_str()), (0, Some("v0.2.0")), "{}", raw);
    let p = &v["payload"];
    assert_eq!(p["changes"], json!([{"kind": "feature", "title": "Add widget command", "surface": "tools", "paths": ["dev/tools/demo/widget.txt"]}]), "{}", p);
    assert_eq!((p["previous"].as_str(), p["compare_url"].as_str()), (Some("v0.1.0"), Some("https://example.test/acme/demo-oss/compare/v0.1.0...v0.2.0")));
    let log = fx.remote(&["show", "main:CHANGELOG.md"]);
    assert!(log.find("## v0.2.0").unwrap() < log.find("## v0.1.0").unwrap() && log.contains("Add widget command") && !log.contains("tidy") && !log.contains("secretproject"), "{}", log);
    // Three closed batches (first release, the unchanged one, this one); accounts/ never counts.
    let improved = fx.events("kiwi.improved");
    let last = improved.last().unwrap();
    assert_eq!((improved.len(), last["closed_by"].as_str(), last["count"].as_i64()), (3, Some("window"), Some(4)), "{}", last);
    let titles: Vec<&Value> = last["commits"].as_array().unwrap().iter().map(|c| &c["title"]).collect();
    assert!(titles.contains(&&Value::Null) && !last.to_string().contains("secretproject"), "{}", last);
    assert!(!last.to_string().contains("Add account"));
}

#[test]
fn a_fresh_home_recovers_the_last_release_from_the_public_repo() {
    let fx = setup("oss5", &["git", "--version"]);
    let (code, v, raw) = fx.oss(&["flush"]);
    assert_eq!((code, v["tag"].as_str()), (0, Some("v0.1.0")), "{}", raw);
    // No state.json: base comes from the Source-Commit trailer and the version from the highest tag, so v0.1.0 is not re-released.
    let (code, v, raw) = fx.oss_at(&fx.k.home.join("box2"), &["flush"]);
    assert_eq!((code, v["reason"].as_str()), (0, Some("no_changes")), "{}", raw);
    fx.commit(&[("dev/tools/demo/parse.txt", "parser\n")], "Fix parser", None);
    let (code, v, raw) = fx.oss_at(&fx.k.home.join("box3"), &["flush"]);
    assert_eq!((code, v["tag"].as_str(), v["payload"]["previous"].as_str()), (0, Some("v0.1.1"), Some("v0.1.0")), "{}", raw);
    assert_eq!(v["payload"]["changes"], json!([{"kind": "fix", "title": "Fix parser", "surface": "tools", "paths": ["dev/tools/demo/parse.txt"]}]), "{}", v);
    assert_eq!(fx.remote(&["tag", "-l"]), "v0.1.0\nv0.1.1");
}

#[test]
fn failing_tests_block_publish_and_queued_events_are_delivered_later() {
    let fx = setup("oss4", &["git", "no-such-subcommand"]);
    std::fs::remove_file(&fx.token_file).unwrap();
    let (code, v, raw) = fx.oss(&["flush"]);
    assert_eq!((code, v["reason"].as_str()), (1, Some("tests_failed")), "{}", raw);
    assert_eq!(v["tests"][0]["passed"], false);
    assert!(v["events"][0]["queued"].is_string(), "{}", v);
    assert_eq!(fx.remote(&["for-each-ref"]), "");
    assert_eq!(fx.oss(&["status"]).1["outbox"].as_array().map(Vec::len), Some(1));
    // Same tree again: no rerun; the outbox goes out first once kiwi-live is reachable.
    std::fs::write(&fx.token_file, BEARER).unwrap();
    let (code, v, raw) = fx.oss(&["flush"]);
    assert_eq!((code, v["reason"].as_str()), (1, Some("tests_failed_same_tree")), "{}", raw);
    assert_eq!(v["delivered"][0]["event"], "kiwi.improved", "{}", v);
    assert_eq!(fx.oss(&["status"]).1["outbox"], json!([]));
    assert_eq!(fx.events("kiwi.improved").len(), 1);
    assert_eq!(fx.remote(&["for-each-ref"]), "");
}
