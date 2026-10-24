//! Single-writer transaction (R6/R8): propose → snapshot @ base → compile + registry checks in the snapshot → gate →
//! commit + push under a fenced lease → confirm remote SHA → atomic activation of the kiwi.db built in the snapshot.
//! Every state gets its own receipt; a resend with the same proposal id never produces a second commit.
use crate::compiler;
use crate::ops::{self, *};
use crate::util::*;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

const LEASE: &str = "kiwi-writer";

fn git(dir: &Path, args: &[&str]) -> Res<String> {
    let o = Command::new("git").arg("-C").arg(dir).args(args)
        .env("GIT_TERMINAL_PROMPT", "0").env("GIT_AUTHOR_NAME", "kiwi").env("GIT_AUTHOR_EMAIL", "kiwi@example.com")
        .env("GIT_COMMITTER_NAME", "kiwi").env("GIT_COMMITTER_EMAIL", "kiwi@example.com").output().map_err(e)?;
    if !o.status.success() { return Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&o.stderr).trim())); }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn fault(point: &str) { if std::env::var("KIWI_FAULT").ok().as_deref() == Some(point) { eprintln!("kiwi: injected fault at {}", point); std::process::exit(3); } }

fn remote_main(repo: &Path) -> Res<String> {
    let s = git(repo, &["ls-remote", "origin", "refs/heads/main"])?;
    Ok(s.split_whitespace().next().unwrap_or("").to_string())
}

fn set_state(c: &rusqlite::Connection, pid: &str, state: &str, remote: &str, detail: &str) -> Res<()> {
    c.execute("UPDATE outbox SET state=? WHERE idempotency_key=?", params![state, format!("write:{}", pid)]).map_err(e)?;
    receipt(c, pid, state, remote, detail)
}

/// Optional local embeddings (box): `kiwi-embed index DB`. Failure keeps FTS-only search, never blocks a write.
pub fn embed(db: &Path) -> Value {
    if std::env::var("KIWI_SEMANTIC").ok().as_deref() == Some("off") { return json!("off"); }
    let exe = std::env::var("KIWI_EMBED").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/bin/kiwi-embed"));
    if !exe.is_file() { return json!("absent"); }
    match Command::new(&exe).arg("index").arg(db).output() {
        Ok(o) if o.status.success() => serde_json::from_slice(&o.stdout).unwrap_or(json!("ok")),
        Ok(o) => json!(format!("failed: {}", String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or(""))),
        Err(x) => json!(format!("failed: {}", x)),
    }
}

pub fn activate(staged: &Path, sha: &str) -> Res<()> {
    let h = home();
    let tmp = h.join("kiwi.db.activating");
    std::fs::copy(staged, &tmp).map_err(e)?;
    fault("before_activate");
    std::fs::rename(&tmp, h.join("kiwi.db")).map_err(e)?; // atomic on same filesystem
    let g = staged.with_extension("graph.json");
    if g.is_file() { let _ = std::fs::copy(&g, h.join("kiwi.graph.json")); }
    let c = ops::open()?;
    meta_set(&c, "active_sha", sha)?; meta_set(&c, "activation_failed", "")?;
    c.execute("DELETE FROM bundle_cache", []).map_err(e)?;
    Ok(())
}

fn gated(snapshot: &Path, files: &[String]) -> Vec<String> {
    let m: toml::Value = std::fs::read_to_string(snapshot.join("kiwi.toml")).ok().and_then(|s| s.parse().ok()).unwrap_or(toml::Value::Table(Default::default()));
    let pats: Vec<String> = m.get("writer").and_then(|w| w.get("gated")).and_then(|g| g.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    files.iter().filter(|f| pats.iter().any(|p| f.starts_with(p.as_str()) || (p.starts_with('*') && f.contains(p.trim_matches('*'))))).cloned().collect()
}

fn run_checks(snapshot: &Path, files: &[String], out_db: &Path) -> Res<Vec<Value>> {
    let reg = std::fs::read_to_string(snapshot.join("checks/registry.toml")).unwrap_or_default();
    let reg: toml::Value = reg.parse().unwrap_or(toml::Value::Table(Default::default()));
    let only: Vec<String> = std::env::var("KIWI_WRITER_CHECKS").map(|s| s.split(',').map(String::from).collect()).unwrap_or_default();
    let mut res = vec![];
    for c in reg.get("check").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
        let id = c.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if id == "kiwi-compile" { continue; } // done in-process
        if !only.is_empty() && !only.contains(&id) { continue; }
        let paths: Vec<String> = c.get("paths").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        let always = c.get("always").and_then(|v| v.as_bool()).unwrap_or(false);
        if !always && !paths.iter().any(|p| files.iter().any(|f| f.starts_with(p.as_str()))) { continue; }
        let cmd: Vec<String> = c.get("cmd").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        if cmd.is_empty() { continue; }
        let cwd = snapshot.join(c.get("cwd").and_then(|v| v.as_str()).unwrap_or("."));
        let st = Command::new(&cmd[0]).args(&cmd[1..]).current_dir(&cwd).env("KIWI_DB", out_db).output().map_err(|x| format!("check {}: {}", id, x))?;
        res.push(json!({"id": id, "ok": st.status.success()}));
        if !st.status.success() { return Err(format!("check_failed:{}:{}", id, String::from_utf8_lossy(&st.stderr).lines().rev().take(3).collect::<Vec<_>>().join(" | "))); }
    }
    Ok(res)
}

pub fn write(a: &Args) -> Res<(Value, i32)> {
    let repo = PathBuf::from(a.req("repo")?);
    let principal = a.req("principal")?;
    let patch_path = PathBuf::from(a.req("patch")?);
    let patch = std::fs::read(&patch_path).map_err(e)?;
    let msg = a.req("message")?;
    let pid = a.get("proposal-id").unwrap_or_else(|| format!("prop-{}", &sha(&patch)[..16]));
    let mut c = ops::open()?;
    // Idempotent resend: resume from the last durable state instead of writing twice.
    let prior: Option<(String, String)> = c.query_row("SELECT state,payload FROM outbox WHERE idempotency_key=?", [format!("write:{}", pid)], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(e)?;
    let staging = home().join("staging"); std::fs::create_dir_all(&staging).map_err(e)?;
    let staged_db = staging.join(format!("{}.db", pid));
    if let Some((state, payload)) = &prior {
        let p: Value = serde_json::from_str(payload).unwrap_or(Value::Null);
        let commit = p["commit"].as_str().unwrap_or("").to_string();
        match state.as_str() {
            "activated" => return Ok((json!({"proposal_id": pid, "status": "activated", "commit": commit, "resent": true}), 0)),
            "rejected" | "conflicted" | "held" => {} // a fresh attempt is allowed below
            "committed" | "published" => {
                let tok = lease_acquire(&c, LEASE, &principal, 300)?;
                let r = remote_main(&repo)?;
                if r != commit && state == "committed" {
                    // response lost before push: push now if our commit is still the tip locally
                    let _ = git(&repo, &["fetch", "-q", "origin", "main"]);
                    let ff = git(&repo, &["merge-base", "--is-ancestor", "origin/main", &commit]).is_ok();
                    if !ff { set_state(&c, &pid, "conflicted", &r, "remote moved before push")?; lease_release(&c, LEASE, tok)?; return Ok((json!({"proposal_id": pid, "status": "conflicted", "remote": r}), 9)); }
                    lease_check(&c, LEASE, tok)?;
                    git(&repo, &["push", "-q", "origin", &format!("{}:refs/heads/main", commit)])?;
                }
                let r = remote_main(&repo)?;
                let published = r == commit || git(&repo, &["merge-base", "--is-ancestor", &commit, &r]).is_ok();
                if !published { lease_release(&c, LEASE, tok)?; return Err("published_unconfirmed".into()); }
                set_state(&c, &pid, "published", &commit, "resume")?;
                if let Err(x) = activate(&staged_db, &commit) { meta_set(&c, "activation_failed", "1")?; lease_release(&c, LEASE, tok)?; return Err(format!("activation_failed:{}", x)); }
                set_state(&c, &pid, "activated", &commit, "resume")?; lease_release(&c, LEASE, tok)?;
                return Ok((json!({"proposal_id": pid, "status": "activated", "commit": commit, "resumed_from": state}), 0));
            }
            _ => {}
        }
    }
    let tok = lease_acquire(&c, LEASE, &principal, 600)?;
    let fail = |c: &rusqlite::Connection, st: &str, detail: String, code: i32| -> Res<(Value, i32)> {
        set_state(c, &pid, st, "", &detail)?; lease_release(c, LEASE, tok)?;
        Ok((json!({"proposal_id": pid, "status": st, "reason": detail}), code))
    };
    {
        let tx = c.transaction().map_err(e)?;
        if prior.is_none() { outbox(&tx, "kiwi.write", "kiwi/main", &json!({"principal": principal, "message": msg}).to_string(), &format!("write:{}", pid))?; }
        receipt(&tx, &pid, "accepted", "", &principal)?;
        tx.commit().map_err(e)?;
    }
    git(&repo, &["fetch", "-q", "origin", "main"])?;
    let remote = git(&repo, &["rev-parse", "origin/main"])?;
    let base = a.get("base").unwrap_or(remote.clone());
    let files: Vec<String> = {
        let o = Command::new("git").args(["apply", "--numstat", "-"]).current_dir(&repo).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().map_err(e)?;
        use std::io::Write; o.stdin.as_ref().unwrap().write_all(&patch).map_err(e)?;
        let out = o.wait_with_output().map_err(e)?;
        String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.split('\t').nth(2).map(String::from)).collect()
    };
    if files.is_empty() { return fail(&c, "rejected", "empty_or_invalid_patch".into(), 2); }
    if let Some(d) = files.iter().find(|f| denied(f)) { return fail(&c, "rejected", format!("denied_path:{}", d), 2); }
    // base check: only retarget when nothing the patch touches changed between base and remote.
    let mut base_used = base.clone();
    if base != remote {
        let changed = git(&repo, &["diff", "--name-only", &base, &remote]).unwrap_or_else(|_| "?".into());
        if changed == "?" || changed.lines().any(|l| files.contains(&l.to_string())) { return fail(&c, "conflicted", format!("base {} != remote {}", base, remote), 9); }
        base_used = remote.clone();
    }
    // snapshot @ remote tip
    let snap = staging.join(format!("snap-{}", pid));
    let _ = std::fs::remove_dir_all(&snap); std::fs::create_dir_all(&snap).map_err(e)?;
    let arc = Command::new("sh").arg("-c").arg(format!("git -C '{}' archive '{}' | tar -x -C '{}'", repo.display(), base_used, snap.display())).status().map_err(e)?;
    if !arc.success() { return fail(&c, "rejected", "snapshot_failed".into(), 2); }
    let ap = Command::new("git").args(["apply", "--whitespace=nowarn", patch_path.to_str().unwrap()]).current_dir(&snap).output().map_err(e)?;
    if !ap.status.success() { return fail(&c, "conflicted", format!("patch_does_not_apply: {}", String::from_utf8_lossy(&ap.stderr).trim()), 9); }
    let g = gated(&snap, &files);
    if !g.is_empty() {
        let ok = a.get("approval-ref").map(|r| c.query_row("SELECT COUNT(*) FROM approvals WHERE id=? AND revoked=0 AND action='kiwi.write' AND (expires_at IS NULL OR expires_at>?)", params![r, now()], |x| x.get::<_, i64>(0)).unwrap_or(0) > 0).unwrap_or(false);
        if !ok { return fail(&c, "held", format!("needs_user_approval:{}", g.join(",")), 11); }
    }
    let comp = match compiler::compile(&snap, &staged_db, "candidate") { Ok(v) => v, Err(x) => return fail(&c, "rejected", x, 4) };
    let checks = match run_checks(&snap, &files, &staged_db) { Ok(v) => v, Err(x) => return fail(&c, "rejected", x, 4) };
    let emb = embed(&staged_db);
    let _ = std::fs::remove_dir_all(&snap);
    // commit in the single shared clone; it must be clean and at the remote tip.
    let head = git(&repo, &["rev-parse", "HEAD"])?;
    let dirty = git(&repo, &["status", "--porcelain", "--untracked-files=no"])?;
    if head != base_used || !dirty.is_empty() {
        if dirty.is_empty() && git(&repo, &["merge-base", "--is-ancestor", "HEAD", &base_used]).is_ok() { git(&repo, &["merge", "-q", "--ff-only", &base_used])?; }
        else { return fail(&c, "conflicted", "shared clone not clean/at remote tip".into(), 9); }
    }
    git(&repo, &["apply", "--index", "--whitespace=nowarn", patch_path.to_str().unwrap()])?;
    git(&repo, &["commit", "-q", "--no-verify", "-m", &format!("{}\n\nKiwi-Proposal: {}\nKiwi-Principal: {}", msg, pid, principal)])?;
    let commit = git(&repo, &["rev-parse", "HEAD"])?;
    // recompile metadata with the real sha (content identical to the checked candidate)
    { let db = rusqlite::Connection::open(&staged_db).map_err(e)?; db.execute("UPDATE meta SET v=? WHERE k='source_git_sha'", [&commit]).map_err(e)?; }
    c.execute("UPDATE outbox SET payload=? WHERE idempotency_key=?", params![json!({"principal": principal, "message": msg, "commit": commit}).to_string(), format!("write:{}", pid)]).map_err(e)?;
    set_state(&c, &pid, "committed", &commit, "")?;
    fault("before_push");
    if let Err(x) = lease_check(&c, LEASE, tok) { return Ok((json!({"proposal_id": pid, "status": "fenced", "reason": x}), 9)); }
    if let Err(x) = git(&repo, &["push", "-q", "origin", "HEAD:refs/heads/main"]) {
        let _ = git(&repo, &["reset", "-q", "--keep", "HEAD~1"]); // undo only our own unpublished commit; patch stays on disk
        return fail(&c, "conflicted", format!("push_rejected: {}", x), 9);
    }
    fault("after_push");
    let r = remote_main(&repo)?;
    if r != commit { return fail(&c, "conflicted", format!("remote {} != commit {}", r, commit), 9); }
    set_state(&c, &pid, "published", &commit, "")?;
    if let Err(x) = activate(&staged_db, &commit) {
        meta_set(&c, "activation_failed", "1")?; lease_release(&c, LEASE, tok)?;
        return Ok((json!({"proposal_id": pid, "status": "published", "activation": "failed_last_known_good", "reason": x}), 5));
    }
    set_state(&c, &pid, "activated", &commit, "")?;
    lease_release(&c, LEASE, tok)?;
    event(&c, "write.activated", &principal, patch.len() as i64, 0, &commit)?;
    Ok((json!({"proposal_id": pid, "status": "activated", "commit": commit, "base": base_used, "files": files, "compile": comp, "checks": checks, "embed": emb}), 0))
}

/// Rebuild kiwi.db from the shared clone's HEAD (must equal remote) and activate atomically.
pub fn refresh(a: &Args) -> Res<Value> {
    let repo = PathBuf::from(a.req("repo")?);
    let head = git(&repo, &["rev-parse", "HEAD"])?;
    let staged = home().join("staging").join(format!("refresh-{}.db", &head[..12.min(head.len())]));
    std::fs::create_dir_all(staged.parent().unwrap()).map_err(e)?;
    let v = compiler::compile(&repo, &staged, &head)?;
    let emb = if a.flag("replica") { json!("replica_fts_only") } else { embed(&staged) };
    activate(&staged, &head)?;
    // Replicas (Mac/Windows) compile from their read-only clone; the box stays the only writer.
    let c = ops::open()?; meta_set(&c, "replica", if a.flag("replica") { "1" } else { "" })?;
    Ok(json!({"activated": head, "replica": a.flag("replica"), "compile": v, "embed": emb}))
}

pub fn status() -> Res<Value> {
    let c = ops::open()?;
    let q = |sql: &str| -> i64 { c.query_row(sql, [], |r| r.get(0)).unwrap_or(0) };
    let lease: Option<(String, i64, i64)> = c.query_row("SELECT holder,fencing_token,expires_at FROM leases WHERE resource=?", [LEASE], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(e)?;
    let by_p: Vec<Value> = { let mut st = c.prepare("SELECT principal, COUNT(*), CAST(AVG(bytes) AS INTEGER) FROM events WHERE kind LIKE 'context_for.%' GROUP BY principal").map_err(e)?;
        let v = st.query_map([], |r| Ok(json!({"principal": r.get::<_, String>(0)?, "briefs": r.get::<_, i64>(1)?, "avg_bytes": r.get::<_, i64>(2)?}))).map_err(e)?.flatten().collect(); v };
    Ok(json!({
        "kiwi_home": home(), "active_sha": meta_get(&c, "active_sha")?, "activation_failed": meta_get(&c, "activation_failed")?.unwrap_or_default() == "1",
        "epoch": epoch(&c)?, "writer_lease": lease.map(|(h, t, x)| json!({"holder": h, "fencing_token": t, "expires_at": x})),
        "outbox_pending": q("SELECT COUNT(*) FROM outbox WHERE state NOT IN ('activated','rejected','conflicted','held')"),
        "tasks_running": q("SELECT COUNT(*) FROM tasks WHERE state='running'"),
        "briefs": {"miss": q("SELECT COUNT(*) FROM events WHERE kind='context_for.miss'"), "hit": q("SELECT COUNT(*) FROM events WHERE kind='context_for.hit'"),
                   "avg_bytes": q("SELECT CAST(AVG(bytes) AS INTEGER) FROM events WHERE kind LIKE 'context_for.%'"),
                   "avg_tokens": q("SELECT CAST(AVG(tokens) AS INTEGER) FROM events WHERE kind LIKE 'context_for.%'")},
        "by_principal": by_p,
    }))
}

/// Copy ops.db with SQLite's online backup (consistent snapshot) to a path outside the box's KIWI_HOME.
pub fn backup(a: &Args) -> Res<Value> {
    let c = ops::open()?;
    let to = PathBuf::from(a.req("to")?);
    c.execute("VACUUM INTO ?", [to.to_str().unwrap()]).map_err(e)?;
    Ok(json!({"backup": to}))
}
