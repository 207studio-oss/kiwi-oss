use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const AGENT: &str = "00000000-0000-4000-8000-1ad35d608dd7";
const OTHER: &str = "00000000-0000-4000-8000-752c8e5b3c4d";
const BRANCH: &str = "kiwi/feature";
const PROJECT: &str = "example/project";
const PAST: &str = "2020-01-02T00:00:00Z";
const FUTURE: &str = "2199-01-01T00:00:00Z";
const SOURCE_TEXT: [&str; 3] = ["SYNTHETIC_LOG_BODY", "SYNTHETIC_TASK", "SYNTHETIC_NEXT"];
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
    kiwi: PathBuf,
    project: PathBuf,
}

struct Run {
    code: i32,
    stdout: Value,
    receipt: Value,
    raw: String,
}

impl Run {
    fn codes(&self) -> Vec<String> {
        self.receipt["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["code"].as_str().unwrap().to_string())
            .collect()
    }

    fn dispositions(&self) -> Vec<(String, String)> {
        self.receipt["claims"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                (
                    c["path"].as_str().unwrap().to_string(),
                    c["disposition"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }
}

impl Fixture {
    fn new() -> Self {
        Self::with_project_remote("https://github.com/example/project.git")
    }

    fn with_project_remote(remote: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "kiwi-finish-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let f = Self {
            kiwi: root.join("kiwi"),
            project: root.join("project"),
            root,
        };
        for (repo, remote, branch) in [
            (
                &f.kiwi,
                "https://github.com/207studio-founder/kiwi.git",
                "main",
            ),
            (&f.project, remote, BRANCH),
        ] {
            let bare = repo.with_extension("git");
            f.git(&f.root, &["init", "-q", "--bare", bare.to_str().unwrap()]);
            f.git(
                &f.root,
                &["init", "-q", "-b", branch, repo.to_str().unwrap()],
            );
            f.git(repo, &["config", "user.name", "Synthetic"]);
            f.git(repo, &["config", "user.email", "synthetic@example.invalid"]);
            f.git(repo, &["remote", "add", "origin", remote]);
            f.git(
                repo,
                &[
                    "config",
                    &format!("url.{}.insteadOf", bare.display()),
                    remote,
                ],
            );
        }
        f.put(&f.kiwi, "README.md", "Kiwi\n");
        f.put(&f.kiwi, "bots/active/README.md", "Board\n");
        f.put(&f.kiwi, "logs/README.md", "Logs\n");
        f.put(&f.project, ".gitignore", "*.log\n");
        f.put(&f.project, "src/area/file.rs", "unchanged\n");
        f.put(&f.project, "src/area-other/file.rs", "unchanged\n");
        f.put(&f.project, "docs/note.md", "unchanged\n");
        for repo in [&f.kiwi, &f.project] {
            f.commit(repo);
            f.git(repo, &["push", "-q", "-u", "origin", "HEAD"]);
        }
        f
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
                "-C",
            ])
            .arg(dir)
            .args(args)
            .env("GIT_ALLOW_PROTOCOL", "file")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().into()
    }

    fn put(&self, repo: &Path, path: &str, text: &str) {
        let file = repo.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }

    fn commit(&self, repo: &Path) {
        self.git(repo, &["add", "--all"]);
        self.git(repo, &["commit", "-qm", "synthetic"]);
    }

    fn publish(&self, repo: &Path) {
        self.commit(repo);
        self.git(repo, &["push", "-q", "origin", "HEAD"]);
    }

    fn claim_text(repository: &str, branch: &str, owner: &str, expires: &str) -> String {
        format!("repo = {repository:?}\nbranch = {branch:?}\npaths = []\nbot = \"Synthetic\"\nagent_id = {owner:?}\ntask = \"SYNTHETIC_TASK\"\nstarted = \"2020-01-01T00:00:00Z\"\nexpires = {expires:?}\nnext = \"SYNTHETIC_NEXT\"\nupdated = \"2020-01-01T00:00:00Z\"\n")
    }

    fn claim(&self, name: &str, repository: &str, branch: &str, owner: &str, expires: &str) {
        self.put(
            &self.kiwi,
            &format!("bots/active/{name}.toml"),
            &Self::claim_text(repository, branch, owner, expires),
        );
        self.publish(&self.kiwi);
    }

    fn receipt_path(&self) -> PathBuf {
        self.root.join(format!(
            "receipt-{}.json",
            NEXT.fetch_add(1, Ordering::SeqCst)
        ))
    }

    fn finish(&self, extra: &[&str]) -> Run {
        self.finish_into(&self.receipt_path(), extra)
    }

    fn finish_into(&self, receipt: &Path, extra: &[&str]) -> Run {
        self.finish_repos(&self.kiwi, &self.project, receipt, extra)
    }

    fn finish_repos(&self, kiwi: &Path, project: &Path, receipt: &Path, extra: &[&str]) -> Run {
        let mut args = vec![
            "finish".to_string(),
            "--kiwi-repo".into(),
            kiwi.display().to_string(),
            "--project-repo".into(),
            project.display().to_string(),
            "--agent-id".into(),
            AGENT.into(),
            "--branch".into(),
            BRANCH.into(),
            "--receipt".into(),
            receipt.display().to_string(),
        ];
        args.extend(extra.iter().map(|a| a.to_string()));
        let out = Command::new(env!("CARGO_BIN_EXE_kiwi-agent"))
            .args(&args)
            .current_dir(&self.root)
            .output()
            .unwrap();
        let raw = fs::read_to_string(receipt).unwrap_or_default();
        Run {
            code: out.status.code().unwrap_or(-1),
            stdout: serde_json::from_slice(&out.stdout).unwrap(),
            receipt: serde_json::from_str(&raw).unwrap_or(Value::Null),
            raw,
        }
    }

    fn snapshot(&self) -> Vec<String> {
        let mut state = Vec::new();
        for repo in [&self.kiwi, &self.project] {
            state.push(self.git(repo, &["status", "--porcelain", "--untracked-files=all"]));
            state.push(self.git(repo, &["rev-parse", "HEAD"]));
            state.push(self.git(repo, &["for-each-ref"]));
            state.push(format!("{:?}", fs::read(repo.join(".git/index")).unwrap()));
        }
        state.push(self.git(&self.kiwi, &["ls-tree", "-r", "HEAD", "--", "bots/active/"]));
        state
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn bare_invocation(args: &[&str]) -> (i32, Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_kiwi-agent"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        serde_json::from_slice(&out.stdout).unwrap(),
    )
}

#[test]
fn finish_without_arguments_prints_usage() {
    for args in [&["finish"][..], &[][..], &["check"][..]] {
        let (code, stdout) = bare_invocation(args);
        assert_eq!(code, 2, "{args:?}");
        assert_eq!(stdout["code"], "usage", "{args:?}");
        assert!(stdout["source"].as_str().unwrap().contains("--branch"));
    }
}

#[test]
fn clean_pushed_released_finishes() {
    let f = Fixture::new();
    f.put(
        &f.kiwi,
        "logs/2026-10-21-feature.md",
        "kiwi/feature merged. SYNTHETIC_LOG_BODY\n",
    );
    f.claim("older", PROJECT, "kiwi/older", AGENT, PAST);
    let receipt = f.receipt_path();
    let run = f.finish_into(&receipt, &["--log", "logs/2026-10-21-feature.md"]);
    assert_eq!(run.code, 0, "{}", run.raw);
    assert_eq!(run.receipt["state"], "finished");
    assert_eq!(run.receipt["findings"], json!([]));
    assert_eq!(run.receipt["upstream_source"], "local_remote_tracking_ref");
    assert_eq!(run.stdout, run.receipt);
    for text in SOURCE_TEXT {
        assert!(!run.raw.contains(text), "{text} copied into receipt");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&receipt).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn own_current_claim_is_release_pending() {
    let f = Fixture::new();
    f.claim("mine", PROJECT, BRANCH, AGENT, FUTURE);
    let run = f.finish(&[]);
    assert_eq!(run.code, 3, "{}", run.raw);
    assert_eq!(run.receipt["state"], "release_pending");
    assert_eq!(run.codes(), ["release_pending"]);
    assert_eq!(
        run.dispositions(),
        [("bots/active/mine.toml".into(), "own_current".into())]
    );
}

#[test]
fn keep_claim_finishes_and_lists_claim() {
    let f = Fixture::new();
    f.claim("mine", PROJECT, BRANCH, AGENT, FUTURE);
    let run = f.finish(&["--keep-claim"]);
    assert_eq!(run.code, 0, "{}", run.raw);
    assert_eq!(run.receipt["state"], "finished");
    let claim = &run.receipt["claims"][0];
    let mut keys: Vec<_> = claim.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["blob", "disposition", "expires_at", "path"]);
    assert_eq!(claim["path"], "bots/active/mine.toml");
    assert_eq!(claim["disposition"], "kept");
    assert_eq!(
        claim["blob"],
        f.git(&f.kiwi, &["rev-parse", "HEAD:bots/active/mine.toml"])
    );
    for text in SOURCE_TEXT {
        assert!(!run.raw.contains(text), "{text} copied into receipt");
    }
}

#[test]
fn old_slug_claim_counts_as_own() {
    for (remote, claimed) in [
        (
            "https://github.com/207studio-founder/kiwi-private.git",
            "207studio-founder/kiwi",
        ),
        (
            "git@github.com:207studio-founder/kiwi.git",
            "207studio-founder/kiwi-private",
        ),
        (
            "https://github.com/207studio-founder/kiwi.git",
            "207studio-founder/kiwi-private",
        ),
        (
            "git@github.com:207studio-founder/kiwi-private.git",
            "207studio-founder/kiwi",
        ),
    ] {
        let f = Fixture::with_project_remote(remote);
        f.claim("mine", claimed, BRANCH, AGENT, FUTURE);
        let run = f.finish(&[]);
        assert_eq!(run.code, 3, "{remote}: {}", run.raw);
        assert_eq!(run.codes(), ["release_pending"]);
    }
}

#[test]
fn other_agent_or_branch_claim_is_ignored() {
    let f = Fixture::new();
    f.claim("other-agent", PROJECT, BRANCH, OTHER, FUTURE);
    f.claim("other-branch", PROJECT, "kiwi/next", AGENT, FUTURE);
    f.claim("other-repo", "example/elsewhere", BRANCH, AGENT, FUTURE);
    let run = f.finish(&[]);
    assert_eq!(run.code, 0, "{}", run.raw);
    assert_eq!(run.receipt["claims"], json!([]));
}

#[test]
fn own_expired_current_claim_is_release_pending() {
    let f = Fixture::new();
    f.claim("mine", PROJECT, BRANCH, AGENT, PAST);
    let run = f.finish(&[]);
    assert_eq!(run.code, 3, "{}", run.raw);
    assert_eq!(
        run.dispositions(),
        [("bots/active/mine.toml".into(), "own_current".into())]
    );
}

#[test]
fn own_expired_claim_elsewhere_is_listed_without_failing() {
    let f = Fixture::new();
    f.claim("older", PROJECT, "kiwi/older", AGENT, PAST);
    f.claim("other-agent-old", PROJECT, "kiwi/older", OTHER, PAST);
    f.claim("other-repo-old", "example/elsewhere", BRANCH, AGENT, PAST);
    let run = f.finish(&[]);
    assert_eq!(run.code, 0, "{}", run.raw);
    assert_eq!(run.receipt["findings"], json!([]));
    assert_eq!(
        run.dispositions(),
        [
            ("bots/active/older.toml".into(), "own_expired".into()),
            (
                "bots/active/other-repo-old.toml".into(),
                "own_expired".into()
            ),
        ]
    );
}

#[test]
fn dirty_tracked_or_untracked_in_scope_is_incomplete() {
    for (path, stage) in [
        ("src/area/file.rs", false),
        ("src/area/new.rs", false),
        ("src/area/staged.rs", true),
    ] {
        let f = Fixture::new();
        f.put(&f.project, path, "changed\n");
        if stage {
            f.git(&f.project, &["add", path]);
        }
        let run = f.finish(&["--path", "src/area/"]);
        assert_eq!(run.code, 4, "{path}: {}", run.raw);
        assert_eq!(run.receipt["state"], "incomplete");
        assert_eq!(
            run.receipt["findings"],
            json!([{"state": "incomplete", "code": "uncommitted_changes", "source": path}])
        );
    }
}

#[test]
fn ignored_and_out_of_scope_changes_pass() {
    let f = Fixture::new();
    f.put(&f.project, "src/area/build.log", "ignored\n");
    f.put(&f.project, "src/area-other/file.rs", "changed\n");
    f.put(&f.project, "docs/new.md", "untracked\n");
    let run = f.finish(&["--path", "src/area"]);
    assert_eq!(run.code, 0, "{}", run.raw);

    let whole = Fixture::new();
    whole.put(&whole.project, "src/area/build.log", "ignored\n");
    let run = whole.finish_into(&whole.project.join("receipt.json"), &[]);
    assert_eq!(run.code, 0, "{}", run.raw);
}

#[test]
fn no_upstream_is_incomplete() {
    let f = Fixture::new();
    f.git(&f.project, &["branch", "--unset-upstream"]);
    let run = f.finish(&[]);
    assert_eq!(run.code, 4, "{}", run.raw);
    assert_eq!(run.codes(), ["no_upstream"]);
    assert_eq!(run.receipt["project_upstream"], Value::Null);
}

#[test]
fn head_ahead_of_upstream_is_incomplete() {
    let f = Fixture::new();
    f.put(&f.project, "src/area/file.rs", "committed but not pushed\n");
    f.commit(&f.project);
    let run = f.finish(&[]);
    assert_eq!(run.code, 4, "{}", run.raw);
    assert_eq!(run.codes(), ["head_not_on_upstream"]);
}

#[test]
fn local_branch_upstream_is_not_synchronized() {
    let project = Fixture::new();
    let kiwi = Fixture::new();
    for (f, repo) in [(&project, &project.project), (&kiwi, &kiwi.kiwi)] {
        f.git(repo, &["branch", "-q", "base"]);
        f.git(repo, &["branch", "-q", "-u", "base"]);
    }
    let run = project.finish(&[]);
    assert_eq!(run.code, 4, "{}", run.raw);
    assert_eq!(run.codes(), ["no_upstream"]);
    let run = kiwi.finish(&[]);
    assert_eq!(run.code, 2, "{}", run.raw);
    assert_eq!(run.codes(), ["kiwi_not_synced"]);
}

#[test]
fn upstream_remote_need_not_be_named_origin() {
    let f = Fixture::new();
    for repo in [&f.kiwi, &f.project] {
        f.git(repo, &["remote", "rename", "origin", "github"]);
        f.git(
            repo,
            &[
                "remote",
                "add",
                "fork",
                "https://github.com/example/fork.git",
            ],
        );
    }
    let run = f.finish(&[]);
    assert_eq!(run.code, 0, "{}", run.raw);
    assert_eq!(run.receipt["project_remote"], "github.com/example/project");

    f.git(&f.project, &["branch", "--unset-upstream"]);
    let run = f.finish(&[]);
    assert_eq!(run.code, 2, "{}", run.raw);
    assert_eq!(run.codes(), ["remote_unresolved"]);
}

#[test]
fn repository_root_is_judged_by_git_not_by_spelling() {
    let f = Fixture::new();
    let run = f.finish_repos(&f.kiwi, &f.project.join("src"), &f.receipt_path(), &[]);
    assert_eq!(run.code, 2, "{}", run.raw);
    assert_eq!(run.codes(), ["repository_root_required"]);
    #[cfg(target_os = "macos")]
    {
        let data = |p: &Path| Path::new("/System/Volumes/Data").join(p.strip_prefix("/").unwrap());
        let run = f.finish_repos(&data(&f.kiwi), &data(&f.project), &f.receipt_path(), &[]);
        assert_eq!(run.code, 0, "{}", run.raw);
    }
}

#[cfg(unix)]
#[test]
fn out_of_scope_paths_are_not_read() {
    use std::os::unix::ffi::OsStrExt;
    let f = Fixture::new();
    let mut entry = format!(
        "100644,{},",
        f.git(&f.project, &["rev-parse", "HEAD:docs/note.md"])
    )
    .into_bytes();
    entry.extend_from_slice(b"docs/\xff.md");
    let added = Command::new("git")
        .arg("-C")
        .arg(&f.project)
        .args(["update-index", "--add", "--cacheinfo"])
        .arg(std::ffi::OsStr::from_bytes(&entry))
        .status()
        .unwrap();
    assert!(added.success());
    f.git(&f.project, &["commit", "-qm", "synthetic"]);
    f.git(&f.project, &["push", "-q", "origin", "HEAD"]);
    let run = f.finish(&["--path", "src/area"]);
    assert_eq!(run.code, 0, "{}", run.raw);
    let run = f.finish(&[]);
    assert_eq!(run.code, 2, "{}", run.raw);
    assert_eq!(run.codes(), ["non_utf8_path"]);
}

#[test]
fn branch_mismatch_is_unconfirmed() {
    let f = Fixture::new();
    f.git(&f.project, &["checkout", "-q", "-b", "kiwi/other"]);
    let run = f.finish(&[]);
    assert_eq!(run.code, 2, "{}", run.raw);
    assert_eq!(run.receipt["state"], "unconfirmed");
    assert_eq!(run.codes(), ["branch_mismatch"]);
}

#[test]
fn kiwi_head_not_on_upstream_is_unconfirmed() {
    let ahead = Fixture::new();
    ahead.put(&ahead.kiwi, "logs/local.md", "kiwi/feature\n");
    ahead.commit(&ahead.kiwi);
    let untracked = Fixture::new();
    untracked.git(&untracked.kiwi, &["branch", "--unset-upstream"]);
    for f in [ahead, untracked] {
        let run = f.finish(&[]);
        assert_eq!(run.code, 2, "{}", run.raw);
        assert_eq!(run.codes(), ["kiwi_not_synced"]);
    }
}

#[test]
fn non_kiwi_repository_is_unconfirmed() {
    let f = Fixture::new();
    f.git(
        &f.kiwi,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/example/notes.git",
        ],
    );
    let run = f.finish(&[]);
    assert_eq!(run.code, 2, "{}", run.raw);
    assert_eq!(run.codes(), ["unverified_kiwi_repository"]);
}

#[test]
fn uncommitted_board_file_is_unconfirmed() {
    let deleted = Fixture::new();
    deleted.claim("mine", PROJECT, BRANCH, AGENT, FUTURE);
    fs::remove_file(deleted.kiwi.join("bots/active/mine.toml")).unwrap();
    let added = Fixture::new();
    added.put(
        &added.kiwi,
        "bots/active/new.toml",
        &Fixture::claim_text(PROJECT, BRANCH, OTHER, FUTURE),
    );
    let edited = Fixture::new();
    edited.claim("mine", PROJECT, BRANCH, AGENT, FUTURE);
    edited.put(
        &edited.kiwi,
        "bots/active/mine.toml",
        &Fixture::claim_text(PROJECT, "kiwi/next", AGENT, FUTURE),
    );
    for (f, path) in [
        (deleted, "bots/active/mine.toml"),
        (added, "bots/active/new.toml"),
        (edited, "bots/active/mine.toml"),
    ] {
        let run = f.finish(&[]);
        assert_eq!(run.code, 2, "{path}: {}", run.raw);
        assert_eq!(
            run.receipt["findings"],
            json!([{"state": "unconfirmed", "code": "source_not_committed", "source": path}])
        );
    }
}

#[test]
fn log_mentioning_branch_passes() {
    let f = Fixture::new();
    f.put(&f.kiwi, "logs/prs/feature.md", "merged kiwi/feature\n");
    f.publish(&f.kiwi);
    let run = f.finish(&["--log", "logs/prs/feature.md"]);
    assert_eq!(run.code, 0, "{}", run.raw);
    assert_eq!(
        run.receipt["log"],
        json!({
            "path": "logs/prs/feature.md",
            "blob": f.git(&f.kiwi, &["rev-parse", "HEAD:logs/prs/feature.md"]),
        })
    );
}

#[test]
fn log_missing_or_unrelated_is_incomplete() {
    let f = Fixture::new();
    f.put(&f.kiwi, "logs/unrelated.md", "merged kiwi/other\n");
    f.publish(&f.kiwi);
    for log in ["logs/absent.md", "logs/unrelated.md"] {
        let run = f.finish(&["--log", log]);
        assert_eq!(run.code, 4, "{log}: {}", run.raw);
        assert_eq!(
            run.receipt["findings"],
            json!([{"state": "incomplete", "code": "log_missing_branch", "source": log}])
        );
        assert_eq!(run.receipt["log"], Value::Null);
    }
}

#[test]
fn uncommitted_log_is_unconfirmed() {
    let f = Fixture::new();
    f.put(&f.kiwi, "logs/edited.md", "draft\n");
    f.publish(&f.kiwi);
    f.put(&f.kiwi, "logs/edited.md", "merged kiwi/feature\n");
    f.put(&f.kiwi, "logs/new.md", "merged kiwi/feature\n");
    for log in ["logs/edited.md", "logs/new.md"] {
        let run = f.finish(&["--log", log]);
        assert_eq!(run.code, 2, "{log}: {}", run.raw);
        assert_eq!(run.codes(), ["source_not_committed"]);
    }
}

#[test]
fn log_outside_logs_dir_is_rejected() {
    let f = Fixture::new();
    f.put(&f.kiwi, "rules/x.md", "kiwi/feature\n");
    f.publish(&f.kiwi);
    for (log, code) in [
        ("rules/x.md", "invalid_log_path"),
        ("logs/../rules/x.md", "invalid_log_path"),
        ("logs/x.txt", "invalid_log_path"),
        ("/tmp/logs/x.md", "invalid_log_path"),
        ("logs", "invalid_log_path"),
        ("logs/accounts/x.md", "source_not_allowed"),
    ] {
        let receipt = f.receipt_path();
        let run = f.finish_into(&receipt, &["--log", log]);
        assert_eq!(run.code, 2, "{log}");
        assert_eq!(run.stdout["code"], code, "{log}");
        assert!(!receipt.exists(), "{log}");
    }
}

#[test]
fn invalid_agent_id_on_same_branch_is_unconfirmed() {
    let f = Fixture::new();
    f.claim("invalid", PROJECT, BRANCH, "not-a-uuid", FUTURE);
    let run = f.finish(&[]);
    assert_eq!(run.code, 2, "{}", run.raw);
    assert_eq!(run.codes(), ["invalid_claim"]);

    let elsewhere = Fixture::new();
    elsewhere.claim("invalid", PROJECT, "kiwi/next", "not-a-uuid", FUTURE);
    let run = elsewhere.finish(&[]);
    assert_eq!(run.code, 0, "{}", run.raw);
}

#[test]
fn unreadable_claim_is_unconfirmed() {
    for text in ["repo = \n", "branch = \"kiwi/feature\"\n"] {
        let f = Fixture::new();
        f.put(&f.kiwi, "bots/active/broken.toml", text);
        f.publish(&f.kiwi);
        let run = f.finish(&[]);
        assert_eq!(run.code, 2, "{text}: {}", run.raw);
        assert_eq!(run.codes(), ["invalid_claim"]);
    }
}

#[test]
fn findings_are_collected_and_the_worst_state_wins() {
    let f = Fixture::new();
    f.claim("mine", PROJECT, BRANCH, AGENT, FUTURE);
    f.put(&f.project, "src/area/file.rs", "changed\n");
    let run = f.finish(&[]);
    assert_eq!(run.code, 4, "{}", run.raw);
    assert_eq!(run.codes(), ["uncommitted_changes", "release_pending"]);

    f.claim("invalid", PROJECT, BRANCH, "not-a-uuid", FUTURE);
    let run = f.finish(&[]);
    assert_eq!(run.code, 2, "{}", run.raw);
    assert_eq!(
        run.codes(),
        ["uncommitted_changes", "invalid_claim", "release_pending"]
    );
}

#[test]
fn finish_never_changes_either_repository() {
    let f = Fixture::new();
    f.claim("mine", PROJECT, BRANCH, AGENT, FUTURE);
    f.put(&f.kiwi, "logs/prs/feature.md", "merged kiwi/feature\n");
    f.publish(&f.kiwi);
    f.put(&f.project, "src/area/file.rs", "changed\n");
    f.put(&f.project, "src/area/new.rs", "untracked\n");
    let before = f.snapshot();
    let run = f.finish(&["--log", "logs/prs/feature.md"]);
    assert_eq!(run.code, 4, "{}", run.raw);
    assert_eq!(f.snapshot(), before);
}

#[test]
fn receipt_cannot_overwrite_existing_or_sensitive_path() {
    let f = Fixture::new();
    let existing = f.root.join("existing.json");
    fs::write(&existing, "keep\n").unwrap();
    let run = f.finish_into(&existing, &[]);
    assert_eq!(run.code, 2);
    assert_eq!(
        run.stdout["findings"][0]["code"],
        "receipt_must_be_new_file"
    );
    assert_eq!(fs::read_to_string(&existing).unwrap(), "keep\n");

    fs::create_dir(f.root.join("secrets")).unwrap();
    for name in ["secrets/receipt.json", "credentials.json", "receipt.pem"] {
        let path = f.root.join(name);
        let run = f.finish_into(&path, &[]);
        assert_eq!(run.code, 2, "{name}");
        assert_eq!(
            run.stdout["findings"][0]["code"], "source_not_allowed",
            "{name}"
        );
        assert!(!path.exists(), "{name}");
    }

    let tracked = f.project.join("docs/note.md");
    fs::remove_file(&tracked).unwrap();
    let run = f.finish_into(&tracked, &[]);
    assert_eq!(run.code, 2);
    assert_eq!(run.stdout["findings"][0]["code"], "receipt_tracked_source");
    assert!(!tracked.exists());
}

#[test]
fn receipt_cannot_replace_tracked_source_through_case_alias() {
    for staged in [false, true] {
        let f = Fixture::new();
        if staged {
            f.git(&f.project, &["rm", "-q", "docs/note.md"]);
            fs::create_dir_all(f.project.join("docs")).unwrap();
        } else {
            fs::remove_file(f.project.join("docs/note.md")).unwrap();
        }
        let run = f.finish_into(&f.project.join("docs/NOTE.md"), &[]);
        assert_eq!(run.code, 2, "{staged}");
        assert_eq!(
            run.stdout["findings"][0]["code"], "receipt_tracked_source",
            "{staged}"
        );
        assert_eq!(
            fs::read_dir(f.project.join("docs")).unwrap().count(),
            0,
            "{staged}"
        );
    }
}
