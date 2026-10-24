//! Layer A: intent & approval ledger. Explicit user instructions outrank inferred prefs, model output and caches.
use crate::ops::*;
use crate::util::*;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

/// Only the user channel can create instructions/approvals. A reference (pasted doc, other bot) never becomes one.
fn require_user(a: &Args) -> Res<String> {
    let p = a.req("principal")?;
    if p != "user" { return Err(format!("forbidden: principal '{}' cannot record instructions or approvals; record it as --kind reference", p)); }
    a.req("message-ref")
}

pub fn scope_overlaps(a: &str, b: &str) -> bool {
    a == "*" || b == "*" || a == b || b.starts_with(&format!("{}/", a)) || a.starts_with(&format!("{}/", b))
}

pub fn active_intents(c: &Connection, scope: &str) -> Res<Vec<Value>> {
    let mut st = c.prepare("SELECT intent_id,kind,revision,goal,scope,exclusions,epoch,message_ref FROM intents WHERE revoked=0 AND kind!='reference' ORDER BY epoch DESC").map_err(e)?;
    let rows = st.query_map([], |r| Ok(json!({"intent_id": r.get::<_, String>(0)?, "kind": r.get::<_, String>(1)?, "revision": r.get::<_, i64>(2)?,
        "goal": r.get::<_, String>(3)?, "scope": r.get::<_, String>(4)?, "exclusions": r.get::<_, String>(5)?, "epoch": r.get::<_, i64>(6)?, "message_ref": r.get::<_, Option<String>>(7)?})))
        .map_err(e)?;
    Ok(rows.filter_map(|r| r.ok()).filter(|v| scope_overlaps(v["scope"].as_str().unwrap_or("*"), scope)).collect())
}

pub fn intent(sub: &str, a: &Args) -> Res<Value> {
    let mut c = open()?;
    let tx = c.transaction().map_err(e)?;
    let out = match sub {
        "add" | "stop" => {
            let kind = if sub == "stop" { "stop".to_string() } else { a.get("kind").unwrap_or("instruction".into()) };
            let mref = if kind == "reference" { a.get("message-ref").unwrap_or_default() } else { require_user(a)? };
            let goal = a.get("goal").unwrap_or_else(|| if kind == "stop" { "stop".into() } else { String::new() });
            if goal.is_empty() { return Err("missing --goal".into()); }
            let scope = a.get("scope").unwrap_or("*".into());
            // References are stored for provenance but never move the epoch: they cannot change what executors may do.
            let ep = if kind == "reference" { epoch(&tx)? } else { bump(&tx, "epoch")? };
            let sup = a.get("supersedes");
            let rev: i64 = match &sup { Some(s) => tx.query_row("SELECT revision FROM intents WHERE intent_id=?", [s], |r| r.get(0)).optional().map_err(e)?.unwrap_or(0) + 1, None => 1 };
            if let Some(s) = &sup { tx.execute("UPDATE intents SET revoked=1 WHERE intent_id=?", [s]).map_err(e)?; }
            let id = a.get("id").unwrap_or_else(|| format!("int-{}-{}", ep, &sha(format!("{}{}{}", goal, scope, now()).as_bytes())[..8]));
            tx.execute("INSERT INTO intents(intent_id,kind,revision,message_ref,goal,scope,exclusions,valid_from,supersedes,epoch) VALUES(?,?,?,?,?,?,?,?,?,?)",
                params![id, kind, rev, mref, goal, scope, a.get("exclusions").unwrap_or_default(), now(), sup, ep]).map_err(e)?;
            let mut affected = vec![];
            if kind != "reference" {
                // User change first: invalidate cached bundles/answers before anything else reads them.
                tx.execute("DELETE FROM bundle_cache", []).map_err(e)?;
                tx.execute("DELETE FROM answer_cache WHERE intent_epoch < ?", [ep]).map_err(e)?;
                let mut st = tx.prepare("SELECT task_id,scope FROM tasks WHERE state IN ('running','assigned')").map_err(e)?;
                let ts: Vec<(String, String)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(e)?.filter_map(|r| r.ok()).collect();
                drop(st);
                for (t, _s) in ts.into_iter().filter(|(_, s)| scope_overlaps(&scope, s)) {
                    let (st_new, eff) = if kind == "stop" { ("cancel_requested", "cancel") } else { ("revise_requested", "revise") };
                    tx.execute("UPDATE tasks SET state=?, updated_at=? WHERE task_id=?", params![st_new, now(), t]).map_err(e)?;
                    outbox(&tx, eff, &t, &json!({"intent_id": id, "epoch": ep}).to_string(), &format!("{}:{}:{}", eff, id, t))?;
                    affected.push(t);
                }
            }
            event(&tx, &format!("intent.{}", kind), "user", 0, 0, &id)?;
            json!({"intent_id": id, "kind": kind, "revision": rev, "epoch": ep, "affected_tasks": affected})
        }
        "revoke" => {
            require_user(a)?;
            let id = a.pos.get(2).cloned().ok_or("intent revoke <id>")?;
            let n = tx.execute("UPDATE intents SET revoked=1 WHERE intent_id=?", [&id]).map_err(e)?;
            if n == 0 { return Err(format!("no intent {}", id)); }
            let ep = bump(&tx, "epoch")?; tx.execute("DELETE FROM bundle_cache", []).map_err(e)?;
            json!({"revoked": id, "epoch": ep})
        }
        "list" => json!({"epoch": epoch(&tx)?, "intents": active_intents(&tx, &a.get("scope").unwrap_or("*".into()))?}),
        _ => return Err("intent add|stop|revoke|list".into()),
    };
    tx.commit().map_err(e)?;
    Ok(out)
}

pub fn pref(a: &Args) -> Res<Value> {
    let c = open()?;
    c.execute("INSERT INTO inferred_prefs(statement,evidence,uncertainty) VALUES(?,?,?)",
        params![a.req("statement")?, a.req("evidence")?, a.get("uncertainty").and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.5)]).map_err(e)?;
    // Deliberately no epoch bump and no cache invalidation: inferred prefs cannot override explicit intents.
    Ok(json!({"pref_id": c.last_insert_rowid(), "overrides_intents": false}))
}

pub fn approve(sub: &str, a: &Args) -> Res<Value> {
    let c = open()?;
    let f = |k: &str| a.get(k).unwrap_or_default();
    match sub {
        "add" => {
            let mref = require_user(a)?;
            let id = format!("apr-{}", &sha(format!("{}{}{}{}{}", f("subject"), f("action"), f("resource"), f("recipient"), now()).as_bytes())[..10]);
            let exp = a.get("expires-in").and_then(|s| s.parse::<i64>().ok()).map(|s| now() + s);
            c.execute("INSERT INTO approvals(id,subject,action,resource,recipient,data_scope,cost_limit,message_ref,expires_at,created_at) VALUES(?,?,?,?,?,?,?,?,?,?)",
                params![id, a.req("subject")?, a.req("action")?, a.req("resource")?, f("recipient"), f("data-scope"), f("cost-limit"), mref, exp, now()]).map_err(e)?;
            Ok(json!({"approval_id": id, "grants_external_permission": false}))
        }
        "check" => {
            // Exact-scope reuse only. Missing answer is never approval.
            let id: Option<String> = c.query_row("SELECT id FROM approvals WHERE revoked=0 AND subject=? AND action=? AND resource=? AND recipient=? AND data_scope=? AND (expires_at IS NULL OR expires_at>?) ORDER BY created_at DESC",
                params![a.req("subject")?, a.req("action")?, a.req("resource")?, f("recipient"), f("data-scope"), now()], |r| r.get(0)).optional().map_err(e)?;
            Ok(json!({"approved": id.is_some(), "approval_id": id}))
        }
        "revoke" => {
            require_user(a)?;
            let id = a.pos.get(2).cloned().ok_or("approve revoke <id>")?;
            c.execute("UPDATE approvals SET revoked=1 WHERE id=?", [&id]).map_err(e)?;
            bump(&c, "epoch")?;
            Ok(json!({"revoked": id}))
        }
        _ => Err("approve add|check|revoke".into()),
    }
}

/// Task lifecycle with epoch + fencing so late results from cancelled/revised workers are rejected.
pub fn task(sub: &str, a: &Args) -> Res<(Value, i32)> {
    let mut c = open()?;
    let tx = c.transaction().map_err(e)?;
    let id = a.req("task-id")?;
    let row: Option<(String, i64, i64, String)> = tx.query_row("SELECT scope,intent_epoch,fencing_token,state FROM tasks WHERE task_id=?", [&id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional().map_err(e)?;
    let res = match sub {
        "start" => {
            let ep = epoch(&tx)?;
            let tok = bump(&tx, "task_fence")?;
            let scope = a.get("scope").unwrap_or("*".into());
            tx.execute("INSERT INTO tasks(task_id,principal,scope,intent_epoch,fencing_token,state,updated_at) VALUES(?,?,?,?,?, 'running', ?) ON CONFLICT(task_id) DO UPDATE SET intent_epoch=excluded.intent_epoch,fencing_token=excluded.fencing_token,state='running',updated_at=excluded.updated_at",
                params![id, a.get("principal").unwrap_or("unknown".into()), scope, ep, tok, now()]).map_err(e)?;
            (json!({"task_id": id, "intent_epoch": ep, "fencing_token": tok, "intents": active_intents(&tx, &scope)?}), 0)
        }
        "guard" | "finish" => {
            let (scope, tep, tok, state) = row.ok_or(format!("no task {}", id))?;
            let ft: i64 = a.req("fencing-token")?.parse().map_err(e)?;
            let newer: i64 = tx.query_row("SELECT COUNT(*) FROM intents WHERE kind!='reference' AND epoch>? AND (scope='*' OR scope=? OR ? LIKE scope||'/%' OR scope LIKE ?||'/%')", params![tep, scope, scope, scope], |r| r.get(0)).map_err(e)?;
            let verdict = if ft != tok { "fenced" } else if state == "cancel_requested" || state == "cancelled" { "cancelled" } else if newer > 0 || state == "revise_requested" { "revise" } else { "ok" };
            let code = match verdict { "ok" => 0, "revise" => 10, _ => 9 };
            if sub == "finish" {
                let status = a.get("status").unwrap_or("done".into());
                if !matches!(status.as_str(), "done" | "failed" | "cancelled" | "rollback_complete") { return Err("status done|failed|cancelled|rollback_complete".into()); }
                // A cancelled worker may still report what it did (cancelled/rollback_complete), but cannot report "done".
                let accepted = verdict == "ok" || (verdict == "cancelled" && ft == tok && status != "done");
                if accepted {
                    tx.execute("UPDATE tasks SET state=?, updated_at=? WHERE task_id=?", params![status, now(), id]).map_err(e)?;
                    receipt(&tx, &id, &format!("task.{}", status), "", &a.get("evidence").unwrap_or_default())?;
                }
                (json!({"task_id": id, "accepted": accepted, "verdict": verdict, "status": status}), if accepted { 0 } else { code.max(9) })
            } else { (json!({"task_id": id, "verdict": verdict}), code) }
        }
        _ => return Err("task start|guard|finish".into()),
    };
    tx.commit().map_err(e)?;
    Ok(res)
}
