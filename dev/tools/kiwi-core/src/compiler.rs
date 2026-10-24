//! Layer D: compile a source snapshot (rules/, bots/, bots/skills/, checks/registry.toml, kiwi.toml) into kiwi.db.
//! kiwi.db is regenerable; it is built in a candidate snapshot and only activated by atomic rename (writer.rs).
use crate::util::*;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

pub const COMPILER_VERSION: &str = "2.0.0";

#[derive(Clone, Default, Debug)]
pub struct Node {
    pub id: String, pub kind: String, pub path: String, pub title: String, pub summary: String, pub body: String,
    pub authority: String, pub mandatory: bool, pub actions: Vec<String>, pub paths: Vec<String>, pub principals: Vec<String>,
    pub supersedes: Vec<String>, pub conflicts: Vec<String>, pub owner: String, pub valid_from: String, pub priority: i64, pub hash: String,
}

pub fn authority_rank(a: &str) -> i64 { match a { "user" => 4, "platform" => 3, "kiwi" => 2, "bot" => 1, _ => 0 } }

fn fm_list(v: &str) -> Vec<String> {
    let v = v.trim();
    if v.starts_with('[') { serde_json::from_str::<Vec<String>>(v).unwrap_or_else(|_| v.trim_matches(|c| c == '[' || c == ']').split(',').map(|s| s.trim().trim_matches('"').to_string()).filter(|s| !s.is_empty()).collect()) }
    else if v.is_empty() { vec![] } else { vec![v.to_string()] }
}

/// Minimal frontmatter: `key: value` lines, JSON-ish lists. Returns (map, body).
pub fn frontmatter(t: &str) -> (HashMap<String, String>, String) {
    let mut m = HashMap::new();
    if let Some(rest) = t.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---\n") {
            for l in rest[..end].lines() {
                if let Some((k, v)) = l.split_once(':') { if !l.starts_with(' ') && !l.trim_start().starts_with('#') { m.insert(k.trim().to_string(), v.split(" #").next().unwrap_or("").trim().to_string()); } }
            }
            return (m, rest[end + 5..].to_string());
        }
    }
    (m, t.to_string())
}

fn title_summary(body: &str) -> (String, String) {
    let mut title = String::new(); let mut summary = String::new();
    for l in body.lines() {
        let s = l.trim();
        if s.is_empty() { continue; }
        if title.is_empty() && s.starts_with('#') { title = s.trim_start_matches('#').trim().to_string(); continue; }
        if summary.is_empty() && !s.starts_with('#') { summary = s.chars().take(240).collect(); break; }
    }
    (title, summary)
}

pub fn links(body: &str) -> Vec<String> {
    // markdown links to *.md, relative
    let mut out = vec![]; let b = body.as_bytes(); let mut i = 0;
    while let Some(p) = body[i..].find("](") {
        let s = i + p + 2;
        if let Some(q) = body[s..].find(')') {
            let target = &body[s..s + q];
            if !target.starts_with("http") && target.split('#').next().unwrap_or("").ends_with(".md") { out.push(target.split('#').next().unwrap().to_string()); }
            i = s + q;
        } else { break; }
        if i >= b.len() { break; }
    }
    out
}

fn norm(base: &Path, rel_from: &str, target: &str) -> String {
    let dir = Path::new(rel_from).parent().unwrap_or(Path::new(""));
    let mut parts: Vec<String> = vec![];
    for c in dir.join(target).components() {
        match c { std::path::Component::ParentDir => { parts.pop(); } std::path::Component::Normal(x) => parts.push(x.to_string_lossy().to_string()), _ => {} }
    }
    let _ = base; parts.join("/")
}

pub struct Compiled { pub nodes: Vec<Node>, pub edges: Vec<(String, String, String, String)>, pub checks: Vec<Value>, pub owners: Vec<(String, String)>, pub errors: Vec<String>, pub warnings: Vec<String> }

pub fn collect(src: &Path) -> Res<Compiled> {
    let manifest: toml::Value = std::fs::read_to_string(src.join("kiwi.toml")).ok().and_then(|s| s.parse().ok()).unwrap_or(toml::Value::Table(Default::default()));
    let mrules = manifest.get("rules").and_then(|v| v.as_table()).cloned().unwrap_or_default();
    let mut files: Vec<(String, String)> = vec![]; // (kind, rel)
    let rd = |d: &str| -> Vec<PathBuf> { let mut v: Vec<PathBuf> = std::fs::read_dir(src.join(d)).map(|r| r.filter_map(|x| x.ok()).map(|x| x.path()).collect()).unwrap_or_default(); v.sort(); v };
    for p in rd("rules") { let n = p.file_name().unwrap().to_string_lossy().to_string(); if n.ends_with(".md") && n != "README.md" { files.push(("rule".into(), format!("rules/{}", n))); } }
    for p in rd("bots") { let n = p.file_name().unwrap().to_string_lossy().to_string(); if n.ends_with(".md") && n.len() > 30 { files.push(("owner".into(), format!("bots/{}", n))); } }
    for p in rd("bots/skills") { if p.join("SKILL.md").is_file() { files.push(("skill".into(), format!("bots/skills/{}/SKILL.md", p.file_name().unwrap().to_string_lossy()))); } }
    let mut nodes = vec![]; let mut errors = vec![]; let mut warnings = vec![]; let mut seen = HashSet::new();
    for (kind, rel) in &files {
        if denied(rel) { errors.push(format!("denied_path_in_source:{}", rel)); continue; }
        let t = std::fs::read_to_string(src.join(rel)).map_err(e)?;
        let (fm, body) = frontmatter(&t);
        let id = fm.get("id").cloned().unwrap_or_else(|| match kind.as_str() {
            "skill" => rel.split('/').nth(2).unwrap().to_string(),
            _ => Path::new(rel).file_stem().unwrap().to_string_lossy().to_string() });
        if !seen.insert(id.clone()) { errors.push(format!("duplicate_id:{}", id)); continue; }
        let m = mrules.get(&id).and_then(|v| v.as_table()).cloned().unwrap_or_default();
        let ms = |k: &str| fm.get(k).cloned().or_else(|| m.get(k).map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())));
        let ml = |k: &str| fm.get(k).map(|v| fm_list(v)).or_else(|| m.get(k).and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())).unwrap_or_default();
        let (title, mut summary) = title_summary(&body);
        if let Some(d) = fm.get("description").or_else(|| fm.get("summary")) { summary = d.trim_matches('"').to_string(); }
        let mandatory = ms("mandatory").map(|v| v == "true").unwrap_or(false);
        let principals = if kind == "owner" { vec![id.clone()] } else { ml("principals") };
        nodes.push(Node { id: id.clone(), kind: kind.clone(), path: rel.clone(), title, summary, hash: sha(body.trim().as_bytes()), body: body.trim().to_string(),
            authority: ms("authority").unwrap_or_else(|| if kind == "owner" { "kiwi".into() } else { "user".into() }),
            mandatory: mandatory || kind == "owner", actions: ml("actions"), paths: ml("paths"), principals, supersedes: ml("supersedes"), conflicts: ml("conflicts"),
            owner: ms("owner").unwrap_or_default(), valid_from: ms("valid_from").unwrap_or_default(), priority: ms("priority").and_then(|s| s.parse().ok()).unwrap_or(50) });
    }
    if let Some(list) = manifest.get("compile").and_then(|c| c.get("mandatory")).and_then(|v| v.as_array()) {
        for x in list.iter().filter_map(|x| x.as_str()) {
            match nodes.iter_mut().find(|n| n.id == x) { Some(n) => n.mandatory = true, None => errors.push(format!("mandatory_missing:{}", x)) }
        }
    }
    // edges
    let by_path: HashMap<String, String> = nodes.iter().map(|n| (n.path.clone(), n.id.clone())).collect();
    let ids: HashSet<String> = nodes.iter().map(|n| n.id.clone()).collect();
    let mut edges = vec![];
    for n in &nodes {
        for l in links(&n.body) {
            let tgt = norm(src, &n.path, &l);
            if let Some(t) = by_path.get(&tgt) { edges.push((n.id.clone(), t.clone(), "links".into(), "".into())); }
            else if tgt.starts_with("rules/") && !src.join(&tgt).exists() { errors.push(format!("broken_link:{}->{}", n.path, l)); }
        }
        for s in &n.supersedes { if ids.contains(s) { edges.push((n.id.clone(), s.clone(), "supersedes".into(), "".into())); } else { warnings.push(format!("supersedes_unknown:{}->{}", n.id, s)); } }
        for s in &n.conflicts { if ids.contains(s) { edges.push((n.id.clone(), s.clone(), "conflicts".into(), "".into())); } }
    }
    // exact-duplicate bodies → relation only, never merged (R4)
    let mut byh: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for n in &nodes { byh.entry(n.hash.clone()).or_default().push(n.id.clone()); }
    for (_, v) in byh.iter().filter(|(_, v)| v.len() > 1) { for w in v.windows(2) { edges.push((w[0].clone(), w[1].clone(), "duplicate_candidate".into(), "basis:hash".into())); } }
    // checks registry (R5): only reviewed commands; never executes proposal text.
    let mut checks = vec![];
    if let Ok(s) = std::fs::read_to_string(src.join("checks/registry.toml")) {
        let reg: toml::Value = s.parse().map_err(|x| format!("checks/registry.toml: {}", x))?;
        for c in reg.get("check").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
            let j: Value = serde_json::to_value(&c).map_err(e)?;
            if j["id"].as_str().is_none() || j["cmd"].as_array().is_none() || j["version"].as_i64().is_none() { errors.push(format!("bad_check:{}", j)); continue; }
            checks.push(j);
        }
    }
    let mut owners = vec![];
    if let Some(t) = manifest.get("owners").and_then(|v| v.as_table()) { for (k, v) in t { if let Some(s) = v.as_str() { owners.push((k.clone(), s.to_string())); } } }
    Ok(Compiled { nodes, edges, checks, owners, errors, warnings })
}

pub fn compile(src: &Path, out: &Path, git_sha: &str) -> Res<Value> {
    let c = collect(src)?;
    if !c.errors.is_empty() { return Err(format!("compile_failed: {}", c.errors.join("; "))); }
    let _ = std::fs::remove_file(out);
    let db = Connection::open(out).map_err(e)?;
    db.execute_batch(r#"
CREATE TABLE meta(k TEXT PRIMARY KEY, v TEXT);
CREATE TABLE nodes(id TEXT PRIMARY KEY, kind TEXT, path TEXT, title TEXT, summary TEXT, body TEXT, authority TEXT, mandatory INTEGER,
  actions TEXT, paths TEXT, principals TEXT, owner TEXT, valid_from TEXT, priority INTEGER, hash TEXT, bytes INTEGER);
CREATE TABLE edges(src TEXT, dst TEXT, type TEXT, basis TEXT);
CREATE TABLE checks(id TEXT PRIMARY KEY, version INTEGER, spec TEXT);
CREATE TABLE owners(prefix TEXT PRIMARY KEY, bot TEXT);
CREATE VIRTUAL TABLE fts USING fts5(id UNINDEXED, title, summary, body, tokenize='trigram');
"#).map_err(e)?;
    let tx = db.unchecked_transaction().map_err(e)?;
    for n in &c.nodes {
        tx.execute("INSERT INTO nodes VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)", params![n.id, n.kind, n.path, n.title, n.summary, n.body, n.authority, n.mandatory as i64,
            json!(n.actions).to_string(), json!(n.paths).to_string(), json!(n.principals).to_string(), n.owner, n.valid_from, n.priority, n.hash, n.body.len() as i64]).map_err(e)?;
        tx.execute("INSERT INTO fts VALUES(?,?,?,?)", params![n.id, n.title, n.summary, n.body]).map_err(e)?;
    }
    for (a, b, t, basis) in &c.edges { tx.execute("INSERT INTO edges VALUES(?,?,?,?)", params![a, b, t, basis]).map_err(e)?; }
    for k in &c.checks { tx.execute("INSERT INTO checks VALUES(?,?,?)", params![k["id"].as_str(), k["version"].as_i64(), k.to_string()]).map_err(e)?; }
    for (p, b) in &c.owners { tx.execute("INSERT INTO owners VALUES(?,?)", params![p, b]).map_err(e)?; }
    let src_digest = sha(c.nodes.iter().map(|n| format!("{}:{}", n.id, n.hash)).collect::<Vec<_>>().join("\n").as_bytes());
    for (k, v) in [("source_git_sha", git_sha), ("compiler_version", COMPILER_VERSION), ("source_digest", &src_digest), ("built_at", &now().to_string())] {
        tx.execute("INSERT INTO meta VALUES(?,?)", params![k, v]).map_err(e)?;
    }
    tx.commit().map_err(e)?;
    drop(db);
    let graph = json!({"source_git_sha": git_sha, "compiler_version": COMPILER_VERSION,
        "nodes": c.nodes.iter().map(|n| json!({"id": n.id, "kind": n.kind, "authority": n.authority, "mandatory": n.mandatory, "bytes": n.body.len()})).collect::<Vec<_>>(),
        "edges": c.edges.iter().map(|(a, b, t, s)| json!({"from": a, "to": b, "type": t, "basis": s})).collect::<Vec<_>>()});
    std::fs::write(out.with_extension("graph.json"), serde_json::to_string(&graph).unwrap()).map_err(e)?;
    let mb: usize = c.nodes.iter().filter(|n| n.mandatory && n.kind != "owner").map(|n| n.body.len()).sum();
    Ok(json!({"db": out, "source_git_sha": git_sha, "nodes": c.nodes.len(), "edges": c.edges.len(), "checks": c.checks.len(),
        "mandatory_global_bytes": mb, "warnings": c.warnings, "duplicate_candidates": c.edges.iter().filter(|x| x.2 == "duplicate_candidate").count()}))
}
