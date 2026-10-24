//! context_for: versioned bundle. Mandatory rules are never truncated (R2); optional context is packed
//! deterministically into the budget (R3); conflicts resolve by authority → scope specificity → supersedes → version (R1).
use crate::compiler::{authority_rank, COMPILER_VERSION};
use crate::intent::active_intents;
use crate::ops;
use crate::util::*;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub fn active_db() -> PathBuf { home().join("kiwi.db") }

pub fn token_estimate(s: &str) -> i64 {
    let (mut ascii, mut other) = (0i64, 0i64);
    for ch in s.chars() { if ch.is_ascii() { ascii += 1 } else { other += 1 } }
    ascii / 4 + other
}

#[derive(Clone)]
struct N { id: String, kind: String, path: String, title: String, summary: String, body: String, authority: String, mandatory: bool,
    actions: Vec<String>, paths: Vec<String>, principals: Vec<String>, valid_from: String, priority: i64 }

fn load(db: &Connection) -> Res<Vec<N>> {
    let mut st = db.prepare("SELECT id,kind,path,title,summary,body,authority,mandatory,actions,paths,principals,valid_from,priority FROM nodes").map_err(e)?;
    let v = st.query_map([], |r| {
        let l = |i: usize| -> rusqlite::Result<Vec<String>> { Ok(serde_json::from_str(&r.get::<_, String>(i)?).unwrap_or_default()) };
        Ok(N { id: r.get(0)?, kind: r.get(1)?, path: r.get(2)?, title: r.get(3)?, summary: r.get(4)?, body: r.get(5)?, authority: r.get(6)?, mandatory: r.get::<_, i64>(7)? == 1,
            actions: l(8)?, paths: l(9)?, principals: l(10)?, valid_from: r.get(11)?, priority: r.get(12)? })
    }).map_err(e)?.filter_map(|x| x.ok()).collect();
    Ok(v)
}

fn applies(n: &N, principal: &str, actions: &[String], paths: &[String]) -> bool {
    (n.principals.is_empty() || n.principals.iter().any(|p| p == principal || p == "*"))
        && (n.actions.is_empty() || n.actions.iter().any(|a| actions.contains(a)))
        && (n.paths.is_empty() || n.paths.iter().any(|p| paths.iter().any(|q| q.starts_with(p.as_str()))))
}
fn specificity(n: &N) -> i64 { (!n.principals.is_empty()) as i64 * 4 + (!n.paths.is_empty()) as i64 * 2 + (!n.actions.is_empty()) as i64 }

pub struct Req { pub principal: String, pub task: String, pub actions: Vec<String>, pub paths: Vec<String>, pub budget: usize,
    pub scores: HashMap<String, f64>, pub tree_hash: String, pub dirty_hash: String, pub task_id: String, pub scope: String, pub no_cache: bool,
    pub question: Option<String>, pub options: Vec<String> }

/// Cache key form of a task: lowercase word set (order, case, punctuation and repeats ignored),
/// so "Deploy the app!" and "the app deploy" share bundle/Decisions cache entries.
pub fn norm_task(t: &str) -> String {
    let mut w: Vec<String> = t.split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_').filter(|x| !x.is_empty()).map(|x| x.to_lowercase()).collect();
    w.sort(); w.dedup(); w.join(" ")
}

pub fn context_for(r: &Req) -> Res<Value> {
    let mut bundle = context_bundle(r)?;
    let disabled = std::env::var("KIWI_CONTEXT_ANSWER").map(|v| v.eq_ignore_ascii_case("off")).unwrap_or(false);
    let (part, detail) = if disabled {
        let mut part = answer_part(None);
        part["needs_user"] = json!({"reason": "disabled"});
        (part, Value::Null)
    } else {
        // Same ask path as kiwi-sprouts context_for: the task is the default question.
        let question = r.question.as_deref().unwrap_or(r.task.as_str());
        let answer = ask_answer(question, &r.options, &r.task, &r.principal);
        (answer_part(Some(&answer)), answer)
    };
    bundle["answer_detail"] = detail;
    if let (Some(target), Value::Object(part)) = (bundle.as_object_mut(), part) {
        for (key, value) in part { target.insert(key, value); }
    }
    Ok(bundle)
}

/// Turn an ask result into the flat decision block kiwi-sprouts context_for returns.
fn answer_part(answer: Option<&Value>) -> Value {
    let result = answer.and_then(Value::as_object);
    let confidence = result.and_then(|r| r.get("confidence")).and_then(Value::as_f64).unwrap_or(0.0);
    let answered = result.and_then(|r| r.get("status")).and_then(Value::as_str) == Some("answered");
    let mut evidence = vec![];
    if let Some(result) = result {
        for key in ["evidence", "decisions"] {
            for item in result.get(key).and_then(Value::as_array).into_iter().flatten() {
                if let Some(id) = item.as_object().and_then(|o| o.get("id")) {
                    if !id.is_null() { evidence.push(id.clone()); }
                } else if !item.is_null() { evidence.push(item.clone()); }
            }
        }
    }
    let needs_user = if !answered || confidence < 0.75 {
        let reason = result.and_then(|r| r.get("reason")).and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("decisions_unavailable");
        let mut need = json!({"reason": reason});
        if let Some(guess) = result.and_then(|r| r.get("best_guess")).filter(|v| !v.is_null()) {
            need["best_guess"] = guess.clone();
        }
        need
    } else { Value::Null };
    json!({
        "answer": if answered { result.and_then(|r| r.get("answer")).cloned().unwrap_or(Value::Null) } else { Value::Null },
        "confidence": confidence,
        "evidence": evidence,
        "needs_user": needs_user,
        "ask_id": result.and_then(|r| r.get("ask_id")).cloned().unwrap_or(Value::Null),
    })
}

fn ask_failure(reason: &str, error: impl std::fmt::Display) -> Value {
    json!({"status": "needs_user", "answer": "오너 확인 필요", "reason": reason, "error": error.to_string()})
}

fn ask_answer(question: &str, options: &[String], task: &str, principal: &str) -> Value {
    let exe = std::env::var_os("KIWI_ASK").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).unwrap_or_default())
            .join(if cfg!(windows) { ".local/bin/kiwi-ask.exe" } else { ".local/bin/kiwi-ask" })
    });
    let mut command = Command::new(exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command.args(["ask", &format!("--question={question}"), "--json"]);
    if !options.is_empty() { command.arg(format!("--options={}", options.join(","))); }
    command.args([format!("--task={task}"), format!("--principal={principal}")])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return ask_failure(if error.kind() == std::io::ErrorKind::NotFound { "ask_unavailable" } else { "error" }, error),
    };
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill(); let _ = child.wait();
        return ask_failure("error", "kiwi-ask stdout unavailable");
    };
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = vec![];
        let result = stdout.read_to_end(&mut bytes).map(|_| bytes).map_err(e);
        let _ = send.send(result);
    });
    let limit = std::env::var("KIWI_ASK_TIMEOUT").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(90);
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() { return ask_failure("error", format!("kiwi-ask exited with {status}")); }
                let remaining = Duration::from_secs(limit).saturating_sub(start.elapsed());
                let output = receive.recv_timeout(remaining).map_err(|error| match error {
                    std::sync::mpsc::RecvTimeoutError::Timeout => format!("kiwi-ask timed out after {limit} seconds"),
                    _ => "kiwi-ask stdout reader failed".to_string(),
                }).and_then(|result| result)
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).map_err(e));
                return match output {
                    Ok(answer) if answer.is_object() => answer,
                    Ok(_) => ask_failure("error", "kiwi-ask returned a non-object answer"),
                    Err(error) => ask_failure("error", error),
                };
            }
            Ok(None) if start.elapsed() < Duration::from_secs(limit) => std::thread::sleep(Duration::from_millis(10)),
            outcome => {
                let _ = child.kill(); let _ = child.wait();
                return ask_failure("error", match outcome {
                    Err(error) => error.to_string(),
                    _ => format!("kiwi-ask timed out after {limit} seconds"),
                });
            }
        }
    }
}

fn context_bundle(r: &Req) -> Res<Value> {
    let dbp = std::env::var("KIWI_DB").map(PathBuf::from).unwrap_or_else(|_| active_db());
    if !dbp.is_file() { return Err(format!("no_active_db:{} (run kiwi compile/activate)", dbp.display())); }
    let db = Connection::open_with_flags(&dbp, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(e)?;
    let meta = |k: &str| -> String { db.query_row("SELECT v FROM meta WHERE k=?", [k], |x| x.get(0)).unwrap_or_default() };
    let (sha_, compv) = (meta("source_git_sha"), meta("compiler_version"));
    let ops = ops::open()?;
    // 1. user changes first: read the current intent epoch before any cache lookup.
    maybe_refresh(&ops);
    let epoch = ops::epoch(&ops)?;
    let intents = active_intents(&ops, &r.scope)?;
    let stale_flag = ops::meta_get(&ops, "activation_failed")?.unwrap_or_default();
    let replica = ops::meta_get(&ops, "replica")?.unwrap_or_default() == "1";
    let mut sorted_scores: Vec<_> = r.scores.iter().map(|(k, v)| format!("{}={:.4}", k, v)).collect(); sorted_scores.sort();
    let key = sha(json!([PACKER_VERSION, r.principal, norm_task(&r.task), r.actions, r.paths, r.budget, epoch, sha_, compv, r.tree_hash, r.dirty_hash, sorted_scores, r.scope]).to_string().as_bytes());
    if !r.no_cache {
        if let Some(b) = ops.query_row("SELECT bundle FROM bundle_cache WHERE key=? AND epoch=? AND active_sha=?", params![key, epoch, sha_], |x| x.get::<_, String>(0)).optional().map_err(e)? {
            let mut v: Value = serde_json::from_str(&b).map_err(e)?; v["cache"] = json!("hit");
            ops::event(&ops, "context_for.hit", &r.principal, v["bytes"].as_i64().unwrap_or(0), v["token_estimate"].as_i64().unwrap_or(0), &r.task_id)?;
            return Ok(v);
        }
    }
    let nodes = load(&db)?;
    let by_id: HashMap<String, N> = nodes.iter().map(|n| (n.id.clone(), n.clone())).collect();
    let mut superseded: HashSet<String> = HashSet::new();
    let mut conflicts: Vec<(String, String)> = vec![];
    { let mut st = db.prepare("SELECT src,dst,type FROM edges WHERE type IN ('supersedes','conflicts')").map_err(e)?;
      for row in st.query_map([], |x| Ok((x.get::<_, String>(0)?, x.get::<_, String>(1)?, x.get::<_, String>(2)?))).map_err(e)?.flatten() {
          if row.2 == "supersedes" { superseded.insert(row.1); } else { conflicts.push((row.0, row.1)); } } }
    let principal_aliases: Vec<String> = vec![r.principal.clone()];
    let ok = |n: &N| !superseded.contains(&n.id) && principal_aliases.iter().any(|p| applies(n, p, &r.actions, &r.paths));
    // 2. mandatory set (never truncated, never competes for budget)
    let mut mandatory: Vec<&N> = nodes.iter().filter(|n| n.mandatory && ok(n)).collect();
    // 3. conflicts among applicable nodes (R1)
    let mut blocked_actions: Vec<Value> = vec![]; let mut dropped: HashSet<String> = HashSet::new();
    for (a, b) in &conflicts {
        let (Some(x), Some(y)) = (by_id.get(a), by_id.get(b)) else { continue };
        if !(ok(x) && ok(y)) { continue; }
        let kx = (authority_rank(&x.authority), specificity(x), x.valid_from.clone());
        let ky = (authority_rank(&y.authority), specificity(y), y.valid_from.clone());
        if kx > ky { dropped.insert(y.id.clone()); } else if ky > kx { dropped.insert(x.id.clone()); }
        else if x.priority != y.priority { dropped.insert(if x.priority > y.priority { y.id.clone() } else { x.id.clone() }); }
        else {
            let acts: Vec<String> = x.actions.iter().chain(y.actions.iter()).cloned().collect::<HashSet<_>>().into_iter().collect();
            blocked_actions.push(json!({"conflict": [a, b], "blocks_actions": if acts.is_empty() { json!(r.actions) } else { json!(acts) }, "reason": "unresolved_conflict"}));
        }
    }
    mandatory.retain(|n| !dropped.contains(&n.id));
    mandatory.sort_by(|a, b| authority_rank(&b.authority).cmp(&authority_rank(&a.authority)).then(specificity(b).cmp(&specificity(a))).then(a.id.cmp(&b.id)));
    let mset: HashSet<String> = mandatory.iter().map(|n| n.id.clone()).collect();
    // 4. optional candidates: exact FTS first + scope boost; external (Decisions) scores override local ones.
    let mut local: HashMap<String, f64> = HashMap::new();
    let terms: Vec<String> = r.task.split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_').filter(|t| t.chars().count() >= 3).map(|t| t.to_lowercase()).collect();
    for t in &terms {
        let q = format!("\"{}\"", t.replace('"', ""));
        if let Ok(mut st) = db.prepare("SELECT id, bm25(fts, 0, 5.0, 2.0, 1.0) FROM fts WHERE fts MATCH ? ") {
            for (id, s) in st.query_map([&q], |x| Ok((x.get::<_, String>(0)?, x.get::<_, f64>(1)?))).map_err(e)?.flatten() { *local.entry(id).or_insert(0.0) += -s; }
        }
        for n in &nodes { if n.id.contains(t.as_str()) { *local.entry(n.id.clone()).or_insert(0.0) += 5.0; } }
    }
    // Semantic (local embeddings, box): blended with exact scores; never touches mandatory rules.
    let sem = semantic(&dbp, &db, &r.task);
    let sem_used = !sem.is_empty();
    for (id, s) in &sem { if *s > 0.3 { *local.entry(id.clone()).or_insert(0.0) += (s - 0.3) * 20.0; } }
    for n in &nodes { if (!n.actions.is_empty() || !n.paths.is_empty()) && applies(n, &r.principal, &r.actions, &r.paths) { *local.entry(n.id.clone()).or_insert(0.0) += 3.0; } }
    let maxl = local.values().cloned().fold(0.0, f64::max).max(1e-9);
    // Decisions ranking of optional candidates (mandatory never enters). Cached by (task, candidate versions).
    let mut dscores = r.scores.clone();
    let mut decisions_status = if dscores.is_empty() { "off" } else { "given" };
    if dscores.is_empty() && std::env::var("KIWI_DECISIONS").ok().as_deref() != Some("off") {
        let mut pre: Vec<&N> = nodes.iter().filter(|n| !mset.contains(&n.id) && !dropped.contains(&n.id) && !superseded.contains(&n.id) && n.kind != "owner" && local.get(&n.id).copied().unwrap_or(0.0) > 0.0).collect();
        pre.sort_by(|a, b| local[&b.id].partial_cmp(&local[&a.id]).unwrap().then(a.id.cmp(&b.id)));
        pre.truncate(40);
        if !pre.is_empty() {
            let items: Vec<(String, String, String, String)> = pre.iter().map(|n| (n.id.clone(), n.title.clone(), n.summary.clone(), n.body.clone())).collect();
            let (st, sc) = decide_scores(&ops, &r.task, &items);
            decisions_status = st; dscores = sc;
        }
    }
    let have_d = !dscores.is_empty();
    let mut cands: Vec<(f64, &N, &str)> = nodes.iter().filter(|n| !mset.contains(&n.id) && !dropped.contains(&n.id) && !superseded.contains(&n.id) && n.kind != "owner")
        .filter_map(|n| match dscores.get(&n.id) { Some(s) => Some((*s, n, "decide")), None => local.get(&n.id).map(|s| (if have_d { s / maxl * 0.3 } else { s / maxl }, n, "local")) })
        .filter(|(s, _, _)| *s > 0.0).collect();
    cands.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap().then(a.1.id.cmp(&b.1.id)));
    // Omitted refs below this score are weak matches: still listed, but they do not make the bundle partial.
const RELEVANT_FLOOR: f64 = 0.5;
const PACKER_VERSION: u32 = 2;
    // 5. deterministic packer: full body if it fits, else summary, else omitted ref.
    let mut used = 0usize; let mut selected = vec![]; let mut omitted = vec![]; let mut missed_relevant = 0usize;
    for (s, n, basis) in &cands {
        let full = n.body.len(); let summ = n.summary.len() + n.title.len() + 16;
        if used + full <= r.budget { used += full; selected.push(json!({"id": n.id, "kind": n.kind, "path": n.path, "score": (s * 1000.0).round() / 1000.0, "basis": basis, "form": "full", "body": n.body})); }
        else if used + summ <= r.budget { used += summ; selected.push(json!({"id": n.id, "kind": n.kind, "path": n.path, "score": (s * 1000.0).round() / 1000.0, "basis": basis, "form": "summary", "title": n.title, "summary": n.summary})); }
        else { if *s >= RELEVANT_FLOOR { missed_relevant += 1; } omitted.push(json!({"id": n.id, "path": n.path, "score": (s * 1000.0).round() / 1000.0, "uri": format!("kiwi://rule/{}", n.id)})); }
    }
    let mand: Vec<Value> = mandatory.iter().map(|n| json!({"id": n.id, "kind": n.kind, "path": n.path, "authority": n.authority, "body": n.body})).collect();
    // 6. required checks (registry only) and owners by path prefix
    let mut checks = vec![];
    { let mut st = db.prepare("SELECT spec FROM checks ORDER BY id").map_err(e)?;
      for s in st.query_map([], |x| x.get::<_, String>(0)).map_err(e)?.flatten() {
          let j: Value = serde_json::from_str(&s).unwrap_or(Value::Null);
          let ps: Vec<String> = j["paths"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
          if j["always"].as_bool().unwrap_or(false) || ps.iter().any(|p| r.paths.iter().any(|q| q.starts_with(p.as_str()))) {
              checks.push(json!({"id": j["id"], "version": j["version"], "cmd": j["cmd"], "cwd": j["cwd"]})); } } }
    let mut owners = vec![];
    { let mut st = db.prepare("SELECT prefix,bot FROM owners").map_err(e)?;
      for (p, b) in st.query_map([], |x| Ok((x.get::<_, String>(0)?, x.get::<_, String>(1)?))).map_err(e)?.flatten() {
          if r.paths.iter().any(|q| q.starts_with(&p)) { owners.push(json!({"prefix": p, "bot": b})); } } }
    let mand_bytes: usize = mand.iter().map(|m| m["body"].as_str().unwrap_or("").len()).sum::<usize>() + intents.iter().map(|i| i["goal"].as_str().unwrap_or("").len()).sum::<usize>();
    let text_all: String = mand.iter().chain(selected.iter()).map(|m| m["body"].as_str().or(m["summary"].as_str()).unwrap_or("").to_string()).collect();
    let bundle = json!({
        "bundle_id": format!("b-{}", &key[..16]), "source_git_sha": sha_, "compiler_version": compv, "core_version": COMPILER_VERSION, "policy_epoch": epoch,
        "principal": r.principal, "task_id": r.task_id,
        "user_intents": intents, "mandatory_rules": mand, "selected_context": selected, "omitted_refs": omitted,
        "required_checks": checks, "owners": owners, "blocked_actions": blocked_actions,
        "completeness": if missed_relevant == 0 { "complete" } else { "partial" },
        "freshness": if !stale_flag.is_empty() { "stale" } else if replica { "replica" } else { "fresh" },
        "bytes": {"mandatory": mand_bytes, "optional": used, "optional_budget": r.budget},
        "token_estimate": token_estimate(&text_all), "semantic": sem_used, "decisions": decisions_status, "expires_at": now() + 600, "cache": "miss"});
    let total = mand_bytes + used;
    if !decisions_status.starts_with("fallback_api") && decisions_status != "fallback_unknown" {
    ops.execute("INSERT OR REPLACE INTO bundle_cache(key,bundle,epoch,active_sha,created_at) VALUES(?,?,?,?,?)", params![key, bundle.to_string(), epoch, sha_, now()]).map_err(e)?; }
    ops::event(&ops, "context_for.miss", &r.principal, total as i64, bundle["token_estimate"].as_i64().unwrap_or(0), &r.task_id)?;
    Ok(bundle)
}

/// Ask the Decisions scorer (tools/decisions/kiwi-scorer) for per-candidate relevance 0..1.
/// Returns (status, scores): cache_hit | scored | fallback_* (empty scores → hybrid local ranking).
/// A fallback is never silent: stderr names it, and KIWI_DECISIONS_REQUIRED=1 makes it fatal (exit 3).
pub fn decide_scores(ops: &Connection, task: &str, pre: &[(String, String, String, String)]) -> (&'static str, HashMap<String, f64>) {
    let r = decide_scores_inner(ops, task, pre);
    if r.0.starts_with("fallback") {
        eprintln!("kiwi: Decisions unavailable ({}); context is NOT Decisions-ranked. Fix KIWI_SCORER / OPENAI_API_KEY on this machine.", r.0);
        if std::env::var("KIWI_DECISIONS_REQUIRED").ok().as_deref() == Some("1") { std::process::exit(3); }
    }
    r
}

fn decide_scores_inner(ops: &Connection, task: &str, pre: &[(String, String, String, String)]) -> (&'static str, HashMap<String, f64>) {
    let mut ver: Vec<String> = pre.iter().map(|n| format!("{}:{}", n.0, &sha(n.3.as_bytes())[..12])).collect(); ver.sort();
    let key = sha(json!([norm_task(task), ver]).to_string().as_bytes());
    let _ = ops.execute("CREATE TABLE IF NOT EXISTS decision_cache(key TEXT PRIMARY KEY, scores TEXT NOT NULL, created_at INTEGER NOT NULL)", []);
    if let Ok(Some(s)) = ops.query_row("SELECT scores FROM decision_cache WHERE key=?", [&key], |x| x.get::<_, String>(0)).optional() {
        if let Ok(m) = serde_json::from_str::<HashMap<String, f64>>(&s) { return ("cache_hit", m); }
    }
    let exe = std::env::var("KIWI_SCORER").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/bin/kiwi-scorer"));
    if !exe.is_file() { return ("fallback_no_scorer", HashMap::new()); }
    let payload = json!({"task": task, "candidates": pre.iter().map(|n| json!({"id": n.0, "title": n.1, "summary": n.2})).collect::<Vec<_>>()}).to_string();
    let child = std::process::Command::new(exe).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn();
    let Ok(mut child) = child else { return ("fallback_spawn", HashMap::new()) };
    { use std::io::Write; if let Some(mut si) = child.stdin.take() { let _ = si.write_all(payload.as_bytes()); } }
    let out = match child.wait_with_output() { Ok(o) if o.status.success() => o.stdout, _ => return ("fallback_api", HashMap::new()) };
    let m: HashMap<String, f64> = serde_json::from_slice(&out).unwrap_or_default();
    let m: HashMap<String, f64> = m.into_iter().filter(|(k, v)| v.is_finite() && pre.iter().any(|n| &n.0 == k)).map(|(k, v)| (k, v.clamp(0.0, 1.0))).collect();
    if m.is_empty() { return ("fallback_unknown", HashMap::new()); }
    let _ = ops.execute("INSERT OR REPLACE INTO decision_cache VALUES(?,?,?)", params![key, serde_json::to_string(&m).unwrap(), now()]);
    ("scored", m)
}

fn semantic(dbp: &std::path::Path, db: &Connection, task: &str) -> HashMap<String, f64> {
    if std::env::var("KIWI_SEMANTIC").ok().as_deref() == Some("off") || task.trim().is_empty() { return HashMap::new(); }
    let has: i64 = db.query_row("SELECT COUNT(*) FROM sqlite_master WHERE name='vec'", [], |x| x.get(0)).unwrap_or(0);
    if has == 0 { return HashMap::new(); }
    let exe = std::env::var("KIWI_EMBED").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/bin/kiwi-embed"));
    if !exe.is_file() { return HashMap::new(); }
    std::process::Command::new(exe).arg("query").arg(dbp).arg(task).output().ok().filter(|o| o.status.success())
        .and_then(|o| serde_json::from_slice(&o.stdout).ok()).unwrap_or_default()
}

/// Lazy auto-refresh (box): at most every KIWI_REFRESH_SECS (300), spawn `kiwi-refresh` in the background.
/// This brief serves the last-known-good db; the next one sees the new main.
pub fn maybe_refresh(ops: &Connection) {
    if std::env::var("KIWI_AUTOREFRESH").ok().as_deref() == Some("off") { return; }
    let exe = PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/bin/kiwi-refresh");
    if !exe.is_file() { return; }
    let every: i64 = std::env::var("KIWI_REFRESH_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(300);
    let last: i64 = ops::meta_get(ops, "refresh_checked_at").ok().flatten().and_then(|s| s.parse().ok()).unwrap_or(0);
    if now() - last < every { return; }
    let _ = ops::meta_set(ops, "refresh_checked_at", &now().to_string());
    let _ = std::process::Command::new(exe).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
}

/// kiwi://rule/{id}: fetch one node (for omitted_refs), still denying sensitive paths.
pub fn get_rule(id: &str) -> Res<Value> {
    let dbp = std::env::var("KIWI_DB").map(PathBuf::from).unwrap_or_else(|_| active_db());
    let db = Connection::open_with_flags(&dbp, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(e)?;
    db.query_row("SELECT id,kind,path,body FROM nodes WHERE id=?", [id], |x| Ok(json!({"id": x.get::<_, String>(0)?, "kind": x.get::<_, String>(1)?, "path": x.get::<_, String>(2)?, "body": x.get::<_, String>(3)?})))
        .optional().map_err(e)?.ok_or(format!("no rule {}", id))
}

/// kiwi compact: keep the task-relevant chunks of a long working context within `budget` bytes.
/// Pinned: first chunk, last `keep_tail` chunks, and chunks containing "[pin]". Others ranked by Decisions,
/// fallback = most recent first. Output keeps original order.
pub fn compact(task: &str, text: &str, budget: usize, keep_tail: usize) -> Res<Value> {
    let chunks: Vec<String> = text.split("\n\n").map(|c| c.trim_end().to_string()).filter(|c| !c.trim().is_empty()).collect();
    let n = chunks.len();
    let pinned = |i: usize| i == 0 || i + keep_tail >= n || chunks[i].contains("[pin]");
    let ops = ops::open()?;
    let items: Vec<(String, String, String, String)> = (0..n).filter(|&i| !pinned(i)).map(|i| (format!("c{}", i), String::new(), chunks[i].chars().take(600).collect(), chunks[i].clone())).collect();
    let (status, sc) = if items.is_empty() || std::env::var("KIWI_DECISIONS").ok().as_deref() == Some("off") { ("off", HashMap::new()) } else { decide_scores(&ops, task, &items) };
    let mut keep: Vec<bool> = (0..n).map(pinned).collect();
    let mut used: usize = (0..n).filter(|&i| keep[i]).map(|i| chunks[i].len() + 2).sum();
    let mut order: Vec<(f64, usize)> = (0..n).filter(|&i| !keep[i]).map(|i| (sc.get(&format!("c{}", i)).copied().unwrap_or(if sc.is_empty() { i as f64 / n.max(1) as f64 } else { 0.0 }), i)).collect();
    order.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap().then(b.1.cmp(&a.1)));
    for (_, i) in order { if used + chunks[i].len() + 2 <= budget { keep[i] = true; used += chunks[i].len() + 2; } }
    let kept: Vec<&String> = (0..n).filter(|&i| keep[i]).map(|i| &chunks[i]).collect();
    let out = kept.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n\n");
    Ok(json!({"chunks": n, "kept": kept.len(), "dropped": n - kept.len(), "bytes_in": text.len(), "bytes_out": out.len(), "budget": budget,
              "pinned_over_budget": (0..n).filter(|&i| pinned(i)).map(|i| chunks[i].len() + 2).sum::<usize>() > budget, "decisions": status, "text": out}))
}
