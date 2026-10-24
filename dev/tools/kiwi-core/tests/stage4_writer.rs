mod common;
use common::Env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn sh(dir: &Path, cmd: &str) -> String {
    let o = Command::new("sh").arg("-c").arg(cmd).current_dir(dir).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t").output().unwrap();
    assert!(o.status.success(), "{}: {}", cmd, String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

struct Fx { k: Env, root: PathBuf, clone: PathBuf }
fn setup(name: &str) -> Fx {
    let k = Env::new(name);
    let root = k.home.join("fx"); std::fs::create_dir_all(&root).unwrap();
    sh(&root, "git init -q --bare -b main remote.git && git init -q -b main seed && mkdir -p seed/rules && printf '# A\\nrule a\\n' > seed/rules/a.md && printf '[compile]\\nmandatory=[\"a\"]\\n[writer]\\ngated=[\"rules/security\", \"checks/\"]\\n' > seed/kiwi.toml && cd seed && git add -A && git commit -qm init && git remote add origin ../remote.git && git push -q origin main && cd .. && git clone -q remote.git clone");
    Fx { clone: root.join("clone"), root, k }
}
fn patch(fx: &Fx, name: &str, edit: &str) -> PathBuf {
    // build a patch in a scratch clone so the shared clone stays untouched
    let s = fx.root.join(format!("scratch-{}", name));
    sh(&fx.root, &format!("rm -rf {0} && git clone -q remote.git {0}", s.display()));
    sh(&s, edit); sh(&s, "git add -A");
    let p = fx.root.join(format!("{}.patch", name));
    std::fs::write(&p, sh(&s, "git diff --cached --binary") + "\n").unwrap(); p
}
fn write(fx: &Fx, p: &Path, pid: &str, extra: &[&str], fault: Option<&str>) -> (i32, serde_json::Value, String) {
    let mut args = vec!["write", "--repo", fx.clone.to_str().unwrap(), "--principal", "box", "--patch", p.to_str().unwrap(), "--message", "test change", "--proposal-id", pid];
    args.extend_from_slice(extra);
    let mut c = Command::new(env!("CARGO_BIN_EXE_kiwi"));
    c.args(&args).env("KIWI_HOME", &fx.k.home).env_remove("KIWI_DB").env("KIWI_SEMANTIC", "off").env("KIWI_AUTOREFRESH", "off").env("KIWI_DECISIONS", "off");
    if let Some(f) = fault { c.env("KIWI_FAULT", f); }
    let o = c.output().unwrap();
    (o.status.code().unwrap_or(-1), serde_json::from_slice(&o.stdout).unwrap_or(serde_json::Value::Null), String::from_utf8_lossy(&o.stderr).to_string())
}
fn remote_count(fx: &Fx) -> usize { sh(&fx.root, "git --git-dir=remote.git rev-list --count main").parse().unwrap() }
fn remote_head(fx: &Fx) -> String { sh(&fx.root, "git --git-dir=remote.git rev-parse main") }
fn active(fx: &Fx) -> String { fx.k.run(&["status"]).1["active_sha"].as_str().unwrap_or("").to_string() }

#[test]
fn happy_path_commits_pushes_and_activates_matching_db() {
    let fx = setup("w1");
    let p = patch(&fx, "p1", "printf '# B\\nrule b\\n' > rules/b.md");
    let (c, v, err) = write(&fx, &p, "p1", &[], None);
    assert_eq!(c, 0, "{} {}", v, err); assert_eq!(v["status"], "activated");
    assert_eq!(v["commit"].as_str().unwrap(), remote_head(&fx));
    assert_eq!(active(&fx), remote_head(&fx));
    let b = fx.k.run(&["brief", "--principal", "p", "--task", "rule b"]).1;
    assert_eq!(b["source_git_sha"].as_str().unwrap(), remote_head(&fx));
    // resend: no second commit
    let (c, v, _) = write(&fx, &p, "p1", &[], None);
    assert_eq!(c, 0); assert_eq!(v["resent"], true); assert_eq!(remote_count(&fx), 2);
}

#[test]
fn crash_before_push_resumes_with_exactly_one_commit() {
    let fx = setup("w2");
    let p = patch(&fx, "p2", "printf '# C\\nc\\n' > rules/c.md");
    assert_eq!(write(&fx, &p, "p2", &[], Some("before_push")).0, 3);
    assert_eq!(remote_count(&fx), 1);
    let (c, v, e) = write(&fx, &p, "p2", &[], None);
    assert_eq!(c, 0, "{} {}", v, e); assert_eq!(v["resumed_from"], "committed");
    assert_eq!(remote_count(&fx), 2); assert_eq!(active(&fx), remote_head(&fx));
}

#[test]
fn crash_after_push_and_before_activate_keep_last_known_good_then_recover() {
    let fx = setup("w3");
    let p = patch(&fx, "p3", "printf '# D\\nd\\n' > rules/d.md");
    assert_eq!(write(&fx, &p, "p3", &[], Some("after_push")).0, 3);
    assert_eq!(remote_count(&fx), 2); assert_eq!(active(&fx), "");
    assert_eq!(write(&fx, &p, "p3", &[], None).0, 0);
    assert_eq!(remote_count(&fx), 2); assert_eq!(active(&fx), remote_head(&fx));
    let first = remote_head(&fx);
    let p4 = patch(&fx, "p4", "printf '# E\\ne\\n' > rules/e.md");
    assert_eq!(write(&fx, &p4, "p4", &[], Some("before_activate")).0, 3);
    assert_eq!(active(&fx), first, "old db stays active");
    assert_eq!(write(&fx, &p4, "p4", &[], None).0, 0);
    assert_eq!(active(&fx), remote_head(&fx));
}

#[test]
fn invalid_candidate_is_rejected_before_commit() {
    let fx = setup("w4");
    let p = patch(&fx, "bad", "printf '# X\\nsee [y](missing.md)\\n' > rules/x.md");
    let (c, v, _) = write(&fx, &p, "bad", &[], None);
    assert_eq!(c, 4); assert_eq!(v["status"], "rejected"); assert_eq!(remote_count(&fx), 1);
    // accounts/ is writable through kiwi write (account-recording); sensitive paths are not blocked (2026-10-23)
    let p = patch(&fx, "acc", "mkdir -p accounts && echo k > accounts/x.md");
    assert_eq!(write(&fx, &p, "acc", &[], None).1["status"], "activated");
}

#[test]
fn stale_base_on_same_file_conflicts_but_unrelated_remote_change_retargets() {
    let fx = setup("w5");
    let base = remote_head(&fx);
    let p = patch(&fx, "s1", "printf '# A\\nchanged\\n' > rules/a.md");
    let other = patch(&fx, "s2", "printf '# A\\nother\\n' > rules/a.md");
    assert_eq!(write(&fx, &other, "s2", &[], None).0, 0);
    let (c, v, _) = write(&fx, &p, "s1", &["--base", &base], None);
    assert_eq!(c, 9); assert_eq!(v["status"], "conflicted");
    let unrelated = patch(&fx, "s3", "printf '# Z\\nz\\n' > rules/z.md");
    let (c, v, e) = write(&fx, &unrelated, "s3", &["--base", &base], None);
    assert_eq!(c, 0, "{} {}", v, e);
}

#[test]
fn gated_paths_are_held_until_user_approval() {
    let fx = setup("w6");
    let p = patch(&fx, "g", "printf '# S\\nsec\\n' > rules/security-x.md");
    let (c, v, _) = write(&fx, &p, "g", &[], None);
    assert_eq!(c, 11); assert_eq!(v["status"], "held"); assert_eq!(remote_count(&fx), 1);
    let a = fx.k.run(&["approve", "add", "--principal", "user", "--message-ref", "chat:1", "--subject", "kiwi", "--action", "kiwi.write", "--resource", "rules/security-x.md"]).1;
    let (c, v, e) = write(&fx, &p, "g", &["--approval-ref", a["approval_id"].as_str().unwrap()], None);
    assert_eq!(c, 0, "{} {}", v, e);
}

#[test]
fn second_writer_cannot_take_a_live_lease() {
    let fx = setup("w7");
    let p = patch(&fx, "l", "printf '# L\\nl\\n' > rules/l.md");
    assert_eq!(write(&fx, &p, "l", &[], Some("before_push")).0, 3); // crashed holder 'box' keeps the lease
    let mut c = Command::new(env!("CARGO_BIN_EXE_kiwi"));
    let o = c.args(["write", "--repo", fx.clone.to_str().unwrap(), "--principal", "mac", "--patch", p.to_str().unwrap(), "--message", "m", "--proposal-id", "l2"]).env("KIWI_HOME", &fx.k.home).output().unwrap();
    assert_eq!(o.status.code(), Some(2)); assert!(String::from_utf8_lossy(&o.stderr).contains("lease_held"));
}
