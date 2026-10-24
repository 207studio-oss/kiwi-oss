//! ops.db: transactional operational state outside git (R8). Back it up; kiwi.db is regenerable, this is not.
use crate::util::*;
use rusqlite::{params, Connection, OptionalExtension};

pub const SCHEMA: &str = r#"
PRAGMA journal_mode=WAL;
CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS intents(intent_id TEXT PRIMARY KEY, kind TEXT NOT NULL CHECK(kind IN ('instruction','stop','reference')),
  revision INTEGER NOT NULL, message_ref TEXT, goal TEXT NOT NULL, scope TEXT NOT NULL DEFAULT '*', exclusions TEXT NOT NULL DEFAULT '',
  valid_from INTEGER NOT NULL, supersedes TEXT, revoked INTEGER NOT NULL DEFAULT 0, epoch INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS inferred_prefs(id INTEGER PRIMARY KEY, statement TEXT NOT NULL, evidence TEXT NOT NULL, uncertainty REAL NOT NULL);
CREATE TABLE IF NOT EXISTS approvals(id TEXT PRIMARY KEY, subject TEXT NOT NULL, action TEXT NOT NULL, resource TEXT NOT NULL,
  recipient TEXT NOT NULL DEFAULT '', data_scope TEXT NOT NULL DEFAULT '', cost_limit TEXT NOT NULL DEFAULT '', message_ref TEXT NOT NULL,
  expires_at INTEGER, revoked INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS tasks(task_id TEXT PRIMARY KEY, principal TEXT NOT NULL, scope TEXT NOT NULL, intent_epoch INTEGER NOT NULL,
  fencing_token INTEGER NOT NULL, state TEXT NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS steps(step_id TEXT PRIMARY KEY, task_id TEXT NOT NULL, executor TEXT, attempts INTEGER NOT NULL DEFAULT 0, state TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS leases(resource TEXT PRIMARY KEY, holder TEXT NOT NULL, fencing_token INTEGER NOT NULL, expires_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS outbox(id INTEGER PRIMARY KEY, effect TEXT NOT NULL, target TEXT NOT NULL, payload TEXT NOT NULL,
  idempotency_key TEXT UNIQUE NOT NULL, state TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS receipts(id INTEGER PRIMARY KEY, ref TEXT NOT NULL, kind TEXT NOT NULL, remote_ref TEXT NOT NULL DEFAULT '', detail TEXT NOT NULL DEFAULT '', observed_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS bundle_cache(key TEXT PRIMARY KEY, bundle TEXT NOT NULL, epoch INTEGER NOT NULL, active_sha TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS answer_cache(key TEXT PRIMARY KEY, owner TEXT NOT NULL, audience TEXT NOT NULL, permission TEXT NOT NULL, intent_epoch INTEGER NOT NULL,
  source_sha TEXT NOT NULL, question TEXT NOT NULL, answer TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS events(id INTEGER PRIMARY KEY, kind TEXT NOT NULL, principal TEXT NOT NULL DEFAULT '', bytes INTEGER NOT NULL DEFAULT 0,
  tokens INTEGER NOT NULL DEFAULT 0, detail TEXT NOT NULL DEFAULT '', at INTEGER NOT NULL);
"#;

pub fn open() -> Res<Connection> {
    let h = home();
    std::fs::create_dir_all(&h).map_err(e)?;
    let c = Connection::open(h.join("ops.db")).map_err(e)?;
    c.busy_timeout(std::time::Duration::from_secs(10)).map_err(e)?;
    c.execute_batch(SCHEMA).map_err(e)?;
    Ok(c)
}

pub fn meta_get(c: &Connection, k: &str) -> Res<Option<String>> {
    c.query_row("SELECT v FROM meta WHERE k=?", [k], |r| r.get(0)).optional().map_err(e)
}
pub fn meta_set(c: &Connection, k: &str, v: &str) -> Res<()> {
    c.execute("INSERT INTO meta(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v", params![k, v]).map(|_| ()).map_err(e)
}
pub fn epoch(c: &Connection) -> Res<i64> { Ok(meta_get(c, "epoch")?.and_then(|s| s.parse().ok()).unwrap_or(0)) }
/// Monotonic counter bump inside the caller's transaction.
pub fn bump(c: &Connection, k: &str) -> Res<i64> {
    let n = meta_get(c, k)?.and_then(|s| s.parse::<i64>().ok()).unwrap_or(0) + 1;
    meta_set(c, k, &n.to_string())?; Ok(n)
}
pub fn event(c: &Connection, kind: &str, principal: &str, bytes: i64, tokens: i64, detail: &str) -> Res<()> {
    c.execute("INSERT INTO events(kind,principal,bytes,tokens,detail,at) VALUES(?,?,?,?,?,?)", params![kind, principal, bytes, tokens, detail, now()]).map(|_| ()).map_err(e)
}
pub fn receipt(c: &Connection, r: &str, kind: &str, remote: &str, detail: &str) -> Res<()> {
    c.execute("INSERT INTO receipts(ref,kind,remote_ref,detail,observed_at) VALUES(?,?,?,?,?)", params![r, kind, remote, detail, now()]).map(|_| ()).map_err(e)
}
/// Outbox insert is idempotent on key: a resend returns the existing row instead of a second effect.
pub fn outbox(c: &Connection, effect: &str, target: &str, payload: &str, key: &str) -> Res<(i64, bool)> {
    if let Some(id) = c.query_row("SELECT id FROM outbox WHERE idempotency_key=?", [key], |r| r.get(0)).optional().map_err(e)? { return Ok((id, false)); }
    c.execute("INSERT INTO outbox(effect,target,payload,idempotency_key,state,created_at) VALUES(?,?,?,?, 'pending', ?)", params![effect, target, payload, key, now()]).map_err(e)?;
    Ok((c.last_insert_rowid(), true))
}

/// Lease with fencing (R8): token strictly increases on every acquisition; expired leases can be taken over.
pub fn lease_acquire(c: &Connection, resource: &str, holder: &str, ttl: i64) -> Res<i64> {
    let cur: Option<(String, i64, i64)> = c.query_row("SELECT holder,fencing_token,expires_at FROM leases WHERE resource=?", [resource], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(e)?;
    if let Some((h, _, exp)) = &cur { if h != holder && *exp > now() { return Err(format!("lease_held:{}:{}", resource, h)); } }
    let tok = bump(c, &format!("fence:{}", resource))?;
    c.execute("INSERT INTO leases(resource,holder,fencing_token,expires_at) VALUES(?,?,?,?) ON CONFLICT(resource) DO UPDATE SET holder=excluded.holder,fencing_token=excluded.fencing_token,expires_at=excluded.expires_at",
        params![resource, holder, tok, now() + ttl]).map_err(e)?;
    Ok(tok)
}
pub fn lease_check(c: &Connection, resource: &str, token: i64) -> Res<()> {
    let cur: Option<(i64, i64)> = c.query_row("SELECT fencing_token,expires_at FROM leases WHERE resource=?", [resource], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(e)?;
    match cur { Some((t, exp)) if t == token && exp > now() => Ok(()), _ => Err(format!("fenced:{}:{}", resource, token)) }
}
pub fn lease_release(c: &Connection, resource: &str, token: i64) -> Res<()> {
    c.execute("DELETE FROM leases WHERE resource=? AND fencing_token=?", params![resource, token]).map(|_| ()).map_err(e)
}
