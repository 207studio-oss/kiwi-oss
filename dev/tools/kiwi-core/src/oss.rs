//! kiwi oss: a sanitized public copy of Kiwi, configured only by tokens `oss.*` (tokens/tokens.json at the source SHA).
//! The export is built from the git tree at one SHA (never the working dir), redacted, then scanned fail-closed: any finding
//! (secret, identifier, bot name, unlisted binary) stops the release before anything leaves the machine. Event-driven, no
//! timers: the box refresh hook runs `tick` after each activation; once the oldest unreleased change is older than
//! `oss.improved.window_minutes` the batch closes as kiwi.improved and the same run consumes it (export → scan → tests →
//! commit + tag → push → oss.released). `flush` closes the batch now; `--dry-run` stops before commit, push and events.
use crate::util::*;
use regex::{Captures, Regex};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

type Files = BTreeMap<String, Vec<u8>>;

fn text_at(v: &Value, k: &str) -> String { v.pointer(k).and_then(Value::as_str).unwrap_or("").to_string() }
fn list_at(v: &Value, k: &str) -> Vec<String> { v.pointer(k).and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() }
fn int_at(v: &Value, k: &str, d: i64) -> i64 { v.pointer(k).and_then(Value::as_i64).unwrap_or(d) }
fn arr_at(v: &Value, k: &str) -> Vec<Value> { v.pointer(k).and_then(Value::as_array).cloned().unwrap_or_default() }

fn git_out(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Res<Vec<u8>> {
    let o = Command::new("git").arg("-C").arg(dir).args(["-c", "core.quotepath=off"]).args(args).env("GIT_TERMINAL_PROMPT", "0").envs(env.iter().copied()).output().map_err(e)?;
    if !o.status.success() { return Err(format!("git {}: {}", args.iter().take(2).copied().collect::<Vec<_>>().join(" "), String::from_utf8_lossy(&o.stderr).trim())); }
    Ok(o.stdout)
}
fn git(dir: &Path, args: &[&str]) -> Res<String> { git_out(dir, args, &[]).map(|o| String::from_utf8_lossy(&o).trim().to_string()) }
fn blob_at(repo: &Path, sha: &str, path: &str) -> Res<Vec<u8>> { git_out(repo, &["cat-file", "blob", &format!("{}:{}", sha, path)], &[]) }

/// Path patterns: `dir/` = prefix from the root, `**/name/` = any directory segment, `**/*.ext` = suffix, `**/name` = file name anywhere, else exact.
fn hit(path: &str, pat: &str) -> bool {
    match pat.strip_prefix("**/") {
        Some(dir) if dir.ends_with('/') => path.split('/').rev().skip(1).any(|seg| seg == &dir[..dir.len() - 1]),
        Some(name) => match name.strip_prefix('*') { Some(ext) => path.ends_with(ext), None => path.rsplit('/').next() == Some(name) },
        None if pat.ends_with('/') => path.starts_with(pat),
        None => path == pat,
    }
}

struct Rule { re: Regex, replace: String, keep: Vec<String> }
struct Allow { path: String, text: String, sha: String }
struct Cfg {
    v: Value, include: Vec<String>, exclude: Vec<String>, overlay: String, tokens_path: String, redact: Vec<Rule>, scan: Vec<(String, Rule)>,
    block: Vec<String>, names: Option<Regex>, allow: Vec<Allow>, binary_allow: Vec<String>, strip: Vec<Regex>, skip: Vec<Regex>,
}

fn regex(p: &str) -> Res<Regex> { Regex::new(p).map_err(|x| format!("oss regex: {}", x)) }
fn rule(r: &Value, field: &str) -> Res<Rule> { Ok(Rule { re: regex(&text_at(r, field))?, replace: text_at(r, "/replace"), keep: list_at(r, "/keep") }) }
/// Group 1 in a keep list (allowed domain, placeholder prefix, CI home) leaves the match alone.
fn kept(r: &Rule, c: &Captures) -> bool { c.get(1).map_or(false, |g| r.keep.iter().any(|k| k.eq_ignore_ascii_case(g.as_str()))) }

fn load(repo: &Path, sha: &str) -> Res<Cfg> {
    let all: Value = serde_json::from_slice(&blob_at(repo, sha, "tokens/tokens.json")?).map_err(|x| format!("tokens/tokens.json: {}", x))?;
    let v = all.get("oss").cloned().ok_or("tokens oss.* missing")?;
    let redact = arr_at(&v, "/redact").iter().map(|r| rule(r, "/pattern")).collect::<Res<Vec<_>>>()?;
    let scan = arr_at(&v, "/scan/patterns").iter().map(|r| Ok((text_at(r, "/kind"), rule(r, "/pattern")?))).collect::<Res<Vec<_>>>()?;
    if scan.is_empty() { return Err("oss.scan.patterns is empty: refusing to export without a scan".into()); }
    let mut names = vec![];
    let from = text_at(&v, "/scan/names_from");
    if !from.is_empty() {
        let reg: toml::Value = String::from_utf8_lossy(&blob_at(repo, sha, &from)?).parse().map_err(|x| format!("{}: {}", from, x))?;
        let skip: Vec<String> = list_at(&v, "/scan/names_exclude").iter().map(|n| n.to_lowercase()).collect();
        for b in reg.get("bot").and_then(|b| b.as_array()).into_iter().flatten() {
            let past = b.get("past_names").and_then(|p| p.as_array()).cloned().unwrap_or_default();
            for n in b.get("name").into_iter().chain(past.iter()).filter_map(|n| n.as_str()).map(str::trim) {
                // "이름 (Alias)" → 이름, Alias; a non-ASCII note like "(은퇴)" is a status, not a name.
                let (main, note) = n.split_once(" (").map_or((n, ""), |(m, r)| (m.trim(), r.trim_end_matches(')').trim()));
                for x in [main, note].into_iter().filter(|x| x.chars().count() >= 2 && (x == &main || x.is_ascii())) {
                    if !skip.contains(&x.to_lowercase()) { names.push(x.to_string()); }
                }
            }
        }
    }
    names.sort(); names.dedup();
    let word = |n: &str| n.is_ascii() && n.starts_with(|c: char| c.is_alphanumeric()) && n.ends_with(|c: char| c.is_alphanumeric());
    let names = if names.is_empty() { None } else {
        Some(regex(&format!("(?i){}", names.iter().map(|n| if word(n) { format!(r"\b{}\b", regex::escape(n)) } else { regex::escape(n) }).collect::<Vec<_>>().join("|")))?)
    };
    Ok(Cfg {
        include: list_at(&v, "/include"), exclude: list_at(&v, "/exclude"), overlay: text_at(&v, "/overlay_dir"), tokens_path: text_at(&v, "/tokens/path"),
        block: list_at(&v, "/scan/blocklist").into_iter().filter(|b| !b.is_empty()).map(|b| b.to_lowercase()).collect(),
        allow: arr_at(&v, "/scan/allow").iter().map(|a| Allow { path: text_at(a, "/path"), text: text_at(a, "/text"), sha: text_at(a, "/sha256") }).collect(),
        binary_allow: list_at(&v, "/scan/binary_allow"), strip: list_at(&v, "/changelog/strip").iter().map(|p| regex(p)).collect::<Res<_>>()?,
        skip: list_at(&v, "/changelog/skip").iter().map(|p| regex(p)).collect::<Res<_>>()?,
        redact, scan, names, v,
    })
}

/// None = exported; Some(reason) = the exclude pattern that won, or not_included.
fn reason(c: &Cfg, path: &str) -> Option<String> {
    if let Some(x) = c.exclude.iter().find(|x| hit(path, x)) { return Some(x.clone()); }
    if c.include.iter().any(|x| hit(path, x)) { None } else { Some("not_included".into()) }
}
fn export_path(c: &Cfg, path: &str) -> Option<String> {
    if !c.overlay.is_empty() { if let Some(r) = path.strip_prefix(&c.overlay) { return Some(r.to_string()); } }
    reason(c, path).is_none().then(|| path.to_string())
}

fn redact(c: &Cfg, text: &str) -> String {
    let mut t = text.to_string();
    for r in &c.redact {
        t = r.re.replace_all(&t, |m: &Captures| {
            if kept(r, m) { return m[0].to_string(); }
            // Stable placeholder: the same id maps to the same fake id in every file and release.
            if r.replace == "{hash_uuid}" { return format!("00000000-0000-4000-8000-{}", &sha(m[0].to_lowercase().as_bytes())[..12]); }
            let mut out = String::new(); m.expand(&r.replace, &mut out); out
        }).into_owned();
    }
    t
}

fn dedupe(v: &mut Value) {
    match v {
        Value::Array(a) => { let mut seen = BTreeSet::new(); a.retain(|x| x.as_str().map_or(true, |s| seen.insert(s.to_string()))); a.iter_mut().for_each(dedupe) }
        Value::Object(m) => m.values_mut().for_each(dedupe),
        _ => {}
    }
}

/// tokens.json: keep only `tokens.keep` sections (a new section stays private until listed), redact, apply example
/// overrides, dedupe arrays that redaction collapsed. Also returns the dropped section names.
fn tokens(c: &Cfg, raw: &str) -> Res<(String, Vec<String>)> {
    let mut v: Value = serde_json::from_str(raw).map_err(|x| format!("{}: {}", c.tokens_path, x))?;
    let keep = list_at(&c.v, "/tokens/keep");
    let dropped: Vec<String> = v.as_object().into_iter().flat_map(|m| m.keys()).filter(|k| !keep.contains(k)).cloned().collect();
    if let Some(m) = v.as_object_mut() { for k in &dropped { m.remove(k); } }
    let mut v: Value = serde_json::from_str(&redact(c, &v.to_string())).map_err(|x| format!("{} after redaction: {}", c.tokens_path, x))?;
    if let (Some(m), Some(set)) = (v.as_object_mut(), c.v.pointer("/tokens/set").and_then(Value::as_object)) { for (k, x) in set { m.insert(k.clone(), x.clone()); } }
    dedupe(&mut v);
    Ok((serde_json::to_string_pretty(&v).map_err(e)? + "\n", dropped))
}

fn norm(dir: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in rel.split('/') { match seg { "" | "." => {} ".." => { parts.pop()?; } s => parts.push(s) } }
    Some(parts.join("/"))
}

/// Markdown links to files that are not exported become plain text instead of dead links.
fn links(path: &str, text: &str, have: &BTreeSet<String>) -> String {
    let re = Regex::new(r"\[([^\]\n]*)\]\(([^)\s]+)\)").unwrap();
    let dir = path.rsplit_once('/').map(|x| x.0).unwrap_or("");
    re.replace_all(text, |m: &Captures| {
        let t = &m[2];
        if t.contains("://") || t.starts_with('#') || t.starts_with("mailto:") || t.starts_with('/') { return m[0].to_string(); }
        match norm(dir, t.split('#').next().unwrap_or("")) {
            Some(p) if p.is_empty() || have.contains(&p) || have.iter().any(|h| h.starts_with(&format!("{}/", p))) => m[0].to_string(),
            _ => m[1].to_string(),
        }
    }).into_owned()
}

fn blobs(repo: &Path, oids: &[String]) -> Res<Vec<Vec<u8>>> {
    let mut ch = Command::new("git").arg("-C").arg(repo).args(["cat-file", "--batch"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().map_err(e)?;
    let mut stdin = ch.stdin.take().ok_or("cat-file stdin")?;
    let input: String = oids.iter().map(|o| format!("{}\n", o)).collect();
    let writer = std::thread::spawn(move || { let _ = stdin.write_all(input.as_bytes()); });
    let mut r = BufReader::new(ch.stdout.take().ok_or("cat-file stdout")?);
    let mut out = Vec::with_capacity(oids.len());
    for _ in oids {
        let mut head = String::new(); r.read_line(&mut head).map_err(e)?;
        let size: usize = head.trim_end().rsplit(' ').next().and_then(|s| s.parse().ok()).ok_or(format!("cat-file: {}", head.trim()))?;
        let mut buf = vec![0; size + 1]; r.read_exact(&mut buf).map_err(e)?; buf.pop();
        out.push(buf);
    }
    let _ = writer.join(); ch.wait().map_err(e)?;
    Ok(out)
}

struct Export { files: Files, exec: BTreeSet<String>, excluded: BTreeMap<String, usize> }

fn build(repo: &Path, sha: &str, c: &Cfg) -> Res<Export> {
    let raw = git_out(repo, &["ls-tree", "-r", "-z", "--full-tree", sha], &[])?;
    let (mut pick, mut over, mut excluded) = (BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
    for rec in raw.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let rec = String::from_utf8_lossy(rec);
        let (meta, path) = rec.split_once('\t').ok_or("ls-tree: bad record")?;
        let mut m = meta.split(' ');
        let (mode, oid) = (m.next().unwrap_or(""), m.nth(1).unwrap_or(""));
        // Only regular files: symlinks could point outside the export, submodules are other repos.
        let blob = (oid.to_string(), mode == "100755");
        if !c.overlay.is_empty() && path.starts_with(&c.overlay) { if mode.starts_with("100") { over.insert(path[c.overlay.len()..].to_string(), blob); } continue; }
        match reason(c, path) {
            Some(r) => *excluded.entry(r).or_insert(0usize) += 1,
            None if !mode.starts_with("100") => *excluded.entry(format!("mode {}", mode)).or_insert(0) += 1,
            None => { pick.insert(path.to_string(), blob); }
        }
    }
    pick.extend(over);
    let bodies = blobs(repo, &pick.values().map(|(oid, _)| oid.clone()).collect::<Vec<_>>())?;
    let have: BTreeSet<String> = pick.keys().cloned().collect();
    let exec: BTreeSet<String> = pick.iter().filter(|(_, (_, x))| *x).map(|(p, _)| p.clone()).collect();
    let mut files = Files::new();
    for ((path, _), raw) in pick.into_iter().zip(bodies) {
        let body = match String::from_utf8(raw) {
            Ok(t) if !t.contains('\0') => {
                let t = if path == c.tokens_path {
                    let (t, dropped) = tokens(c, &t)?;
                    for k in dropped { *excluded.entry(format!("{}#{}", path, k)).or_insert(0) += 1; }
                    t
                } else { redact(c, &t) };
                (if path.ends_with(".md") { links(&path, &t, &have) } else { t }).into_bytes()
            }
            Ok(t) => t.into_bytes(),
            Err(x) => x.into_bytes(),
        };
        files.insert(path, body);
    }
    // The public rules index lists exported rules only, so it is generated here, not kept by hand in the overlay.
    if let Some(readme) = files.get("rules/README.md").map(|b| String::from_utf8_lossy(b).into_owned()) {
        let rules: Vec<_> = files.iter().filter(|(p, _)| p.starts_with("rules/") && p.ends_with(".md") && p.matches('/').count() == 1 && *p != "rules/README.md")
            .map(|(p, b)| { let (fm, body) = crate::compiler::frontmatter(&String::from_utf8_lossy(b)); crate::lint::RuleFile { id: p[6..p.len() - 3].to_string(), rel: p.clone(), fm, body } })
            .collect();
        files.insert("rules/README.md".into(), crate::lint::apply_index(&readme, &crate::lint::render_index(&rules)).into_bytes());
    }
    Ok(Export { files, exec, excluded })
}

fn scan_line(c: &Cfg, path: &str, n: usize, line: &str, out: &mut Vec<Value>) {
    let mut hits: Vec<(&str, String)> = vec![];
    for (kind, r) in &c.scan { for m in r.re.captures_iter(line) { if !kept(r, &m) { hits.push((kind, m[0].to_string())); } } }
    let low = line.to_lowercase();
    for b in &c.block { if low.contains(b.as_str()) { hits.push(("blocklist", b.clone())); } }
    if let Some(re) = &c.names { for m in re.find_iter(line) { hits.push(("bot_name", m.as_str().to_string())); } }
    for (kind, m) in hits {
        let h = sha(m.as_bytes());
        if c.allow.iter().any(|a| a.path == path && ((!a.text.is_empty() && a.text == m) || (a.sha.len() >= 12 && h.starts_with(&a.sha)))) { continue; }
        // Never the matched value: path, line, kind and a hash prefix (for an exact allow entry) only.
        out.push(json!({"path": path, "line": n, "kind": kind, "match_sha": &h[..16]}));
    }
}
fn scan_text(c: &Cfg, path: &str, text: &str, out: &mut Vec<Value>) { for (i, l) in text.lines().enumerate() { scan_line(c, path, i + 1, l, out); } }
fn scan_files(c: &Cfg, files: &Files) -> Vec<Value> {
    let mut out = vec![];
    for (p, b) in files {
        scan_line(c, p, 0, p, &mut out);
        match std::str::from_utf8(b) {
            Ok(t) if !t.contains('\0') => scan_text(c, p, t, &mut out),
            _ if c.binary_allow.contains(p) => {}
            _ => out.push(json!({"path": p, "line": 0, "kind": "binary"})),
        }
    }
    out
}
fn report(f: &[Value]) -> Value { json!({"passed": f.is_empty(), "count": f.len(), "findings": f.iter().take(200).collect::<Vec<_>>()}) }

fn read_dir_files(root: &Path, rel: &str, out: &mut Files) -> Res<()> {
    for d in std::fs::read_dir(root.join(rel)).map_err(e)? {
        let d = d.map_err(e)?;
        let name = d.file_name().to_string_lossy().to_string();
        let p = if rel.is_empty() { name.clone() } else { format!("{}/{}", rel, name) };
        let t = d.file_type().map_err(e)?;
        if t.is_dir() { if name != ".git" { read_dir_files(root, &p, out)?; } } else { out.insert(p.clone(), std::fs::read(root.join(&p)).map_err(e)?); }
    }
    Ok(())
}
fn write_files(root: &Path, ex: &Export) -> Res<()> {
    for (p, b) in &ex.files {
        let f = root.join(p);
        if let Some(d) = f.parent() { std::fs::create_dir_all(d).map_err(e)?; }
        std::fs::write(&f, b).map_err(e)?;
        #[cfg(unix)]
        { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&f, std::fs::Permissions::from_mode(if ex.exec.contains(p) { 0o755 } else { 0o644 })).map_err(e)?; }
    }
    Ok(())
}

/// Civil date from days since 1970-01-01 (Howard Hinnant), so release timestamps need no date crate.
fn iso(t: i64) -> String {
    let (z, s) = (t.div_euclid(86400) + 719468, t.rem_euclid(86400));
    let (era, doe) = (z.div_euclid(146097), z.rem_euclid(146097));
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (d, m) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", yoe + era * 400 + i64::from(m <= 2), m, d, s / 3600, s % 3600 / 60, s % 60)
}

struct Lock(PathBuf);
impl Lock {
    /// Holder pid inside; a dead holder (no /proc entry, or older than 6h where /proc is absent) is taken over.
    fn take(p: PathBuf) -> Res<Option<Lock>> {
        for _ in 0..2 {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&p) {
                Ok(mut f) => { let _ = write!(f, "{}", std::process::id()); return Ok(Some(Lock(p))); }
                Err(x) if x.kind() == std::io::ErrorKind::AlreadyExists => {
                    let pid = std::fs::read_to_string(&p).unwrap_or_default().trim().to_string();
                    let alive = if Path::new("/proc/self").exists() { !pid.is_empty() && Path::new("/proc").join(&pid).exists() }
                        else { std::fs::metadata(&p).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map_or(false, |d| d.as_secs() < 6 * 3600) };
                    if alive { return Ok(None); }
                    let _ = std::fs::remove_file(&p);
                }
                Err(x) => return Err(e(x)),
            }
        }
        Ok(None)
    }
}
impl Drop for Lock { fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); } }

fn state_path() -> PathBuf { home().join("oss").join("state.json") }
fn load_state() -> Value { std::fs::read(state_path()).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).filter(Value::is_object).unwrap_or_else(|| json!({})) }
fn save_state(v: &Value) -> Res<()> {
    let p = state_path(); std::fs::create_dir_all(p.parent().unwrap()).map_err(e)?;
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(v).map_err(e)?).map_err(e)?;
    std::fs::rename(&tmp, &p).map_err(e)
}

/// kiwi-live's bearer token file (same lookup as kiwi_live.py); the token itself is never printed.
fn token_file() -> PathBuf {
    std::env::var("KIWI_LIVE_TOKEN_FILE").map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".kiwi/secrets/kiwi-live-token"))
}

/// POST {event, data} to kiwi-live /events so every event goes through the one signed bus. Loopback only: the bearer is plain HTTP.
fn post(c: &Cfg, event: &str, data: &Value) -> Res<Value> {
    use std::net::ToSocketAddrs;
    let url = text_at(&c.v, "/events_url");
    let rest = url.strip_prefix("http://").ok_or("oss.events_url must be http://<loopback>:<port>/events")?;
    let (hostport, path) = rest.split_once('/').map(|(h, p)| (h, format!("/{}", p))).unwrap_or((rest, "/".into()));
    let host = hostport.rsplit_once(':').map_or(hostport, |x| x.0).trim_matches(|ch| ch == '[' || ch == ']');
    if !matches!(host, "127.0.0.1" | "localhost" | "::1") { return Err("oss.events_url is not loopback".into()); }
    let token = std::fs::read_to_string(token_file()).map_err(|_| "kiwi-live token file unreadable".to_string())?.trim().to_string();
    let body = json!({"event": event, "data": data}).to_string();
    let addr = hostport.to_socket_addrs().map_err(e)?.next().ok_or("events_url: no address")?;
    let mut s = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(5)).map_err(e)?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(15))).map_err(e)?;
    write!(s, "POST {} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", path, hostport, token, body.len(), body).map_err(e)?;
    let mut resp = String::new(); s.read_to_string(&mut resp).map_err(e)?;
    let code: u16 = resp.split_whitespace().nth(1).and_then(|x| x.parse().ok()).unwrap_or(0);
    let reply = resp.split_once("\r\n\r\n").map_or("", |x| x.1);
    if (200..300).contains(&code) { Ok(serde_json::from_str(reply).unwrap_or(json!({}))) } else { Err(format!("events {}: {}", code, reply.chars().take(200).collect::<String>())) }
}

/// Emit or queue: an unreachable kiwi-live never loses an event; the next tick/flush delivers the outbox first.
fn send(c: &Cfg, st: &mut Value, event: &str, data: Value) -> Value {
    match post(c, event, &data) {
        Ok(r) => json!({"event": event, "emitted": r.get("id").cloned().unwrap_or(Value::Null)}),
        Err(x) => {
            if !st["outbox"].is_array() { st["outbox"] = json!([]); }
            st["outbox"].as_array_mut().unwrap().push(json!({"event": event, "data": data, "error": x, "queued_at": iso(now())}));
            json!({"event": event, "queued": x})
        }
    }
}
fn drain(c: &Cfg, st: &mut Value) -> Vec<Value> {
    let items = st.get_mut("outbox").and_then(Value::as_array_mut).map(std::mem::take).unwrap_or_default();
    let (mut done, mut keep) = (vec![], vec![]);
    for mut it in items {
        match post(c, it["event"].as_str().unwrap_or(""), &it["data"]) {
            Ok(r) => done.push(json!({"event": it["event"], "emitted": r.get("id").cloned().unwrap_or(Value::Null)})),
            Err(x) => { it["error"] = json!(x); keep.push(it); }
        }
    }
    st["outbox"] = json!(keep);
    done
}

struct Change { sha: String, at: i64, subject: String, files: Vec<String> }

/// Source commits since the last released SHA that touch exported paths; `files` are export paths.
fn digest(repo: &Path, c: &Cfg, base: Option<&str>, head: &str) -> Res<Vec<Change>> {
    let range = base.map_or_else(|| head.to_string(), |b| format!("{}..{}", b, head));
    let specs: Vec<String> = c.include.iter().cloned().chain(Some(c.overlay.clone()).filter(|o| !o.is_empty())).collect();
    let mut args = vec!["log", "--format=%x1e%H%x1f%ct%x1f%s", "--name-only", range.as_str(), "--"];
    args.extend(specs.iter().map(String::as_str));
    let mut v = vec![];
    for rec in git(repo, &args)?.split('\x1e').filter(|r| !r.trim().is_empty()) {
        let mut lines = rec.lines();
        let h: Vec<&str> = lines.next().unwrap_or("").splitn(3, '\x1f').collect();
        if h.len() < 3 { continue; }
        let mut files: Vec<String> = lines.filter(|l| !l.is_empty()).filter_map(|l| export_path(c, l)).collect();
        files.sort(); files.dedup();
        v.push(Change { sha: h[0].into(), at: h[1].parse().unwrap_or(0), subject: h[2].into(), files });
    }
    Ok(v)
}

/// Public title of a source commit: redacted, attribution stripped. None when it still trips the scan (a bot name in a
/// subject), so one subject drops out of the changelog instead of blocking every later release.
fn title(c: &Cfg, subject: &str) -> Option<String> {
    let mut t = redact(c, subject);
    for r in &c.strip { t = r.replace_all(&t, "").into_owned(); }
    let t = t.trim().to_string();
    let mut f = vec![];
    scan_line(c, "<title>", 0, &t, &mut f);
    (!t.is_empty() && f.is_empty()).then_some(t)
}
fn kind(c: &Cfg, title: &str) -> String {
    let words: Vec<String> = title.to_lowercase().split(|ch: char| !ch.is_alphanumeric()).map(String::from).collect();
    arr_at(&c.v, "/changelog/kinds").iter().find(|k| list_at(k, "/words").iter().any(|w| words.contains(w))).map_or_else(|| text_at(&c.v, "/changelog/default_kind"), |k| text_at(k, "/kind"))
}

/// User-visible entries only: the commit's exported files must be in the actual OSS diff, inside a surface, not ignored,
/// and its subject must not be housekeeping (changelog.skip).
fn changelog(c: &Cfg, pending: &[&Change], changed: &BTreeSet<String>) -> Vec<Value> {
    let (surfaces, ignore) = (arr_at(&c.v, "/changelog/surfaces"), list_at(&c.v, "/changelog/ignore"));
    let max = int_at(&c.v, "/changelog/max_entries", 30) as usize;
    let (mut seen, mut out) = (BTreeSet::new(), vec![]);
    for ch in pending {
        if c.skip.iter().any(|r| r.is_match(&ch.subject)) { continue; }
        let files: Vec<&String> = ch.files.iter().filter(|f| changed.contains(*f) && !ignore.iter().any(|x| hit(f, x))).collect();
        let Some((surface, paths)) = surfaces.iter().find_map(|sf| {
            let pats = list_at(sf, "/paths");
            let p: Vec<String> = files.iter().filter(|f| pats.iter().any(|x| hit(f, x))).take(5).map(|f| f.to_string()).collect();
            (!p.is_empty()).then(|| (text_at(sf, "/name"), p))
        }) else { continue };
        let Some(t) = title(c, &ch.subject) else { continue };
        if !seen.insert(t.clone()) { continue; }
        out.push(json!({"kind": kind(c, &t), "title": t, "surface": surface, "paths": paths}));
        if out.len() >= max { break; }
    }
    out
}

fn bump(v: &str, minor: bool) -> String {
    let n: Vec<u64> = v.split('.').map(|x| x.parse().unwrap_or(0)).collect();
    let at = |i: usize| n.get(i).copied().unwrap_or(0);
    if minor { format!("{}.{}.0", at(0), at(1) + 1) } else { format!("{}.{}.{}", at(0), at(1), at(2) + 1) }
}

fn bullets(c: &Cfg, changes: &[Value]) -> String {
    if changes.is_empty() { return format!("- {}\n", text_at(&c.v, "/changelog/empty_entry")); }
    changes.iter().map(|x| format!("- [{}/{}] {}\n", x["kind"].as_str().unwrap_or(""), x["surface"].as_str().unwrap_or(""), x["title"].as_str().unwrap_or(""))).collect()
}

/// The publish clone mirrors the remote exactly: fetched, reset, cleaned. Unpushed leftovers from a failed run are dropped.
fn sync(c: &Cfg, clone: &Path) -> Res<()> {
    let (url, br) = (text_at(&c.v, "/remote_url"), text_at(&c.v, "/branch"));
    if !clone.join(".git").is_dir() {
        let _ = std::fs::remove_dir_all(clone);
        let parent = clone.parent().ok_or("oss.clone_dir has no parent")?;
        std::fs::create_dir_all(parent).map_err(e)?;
        git(parent, &["clone", "-q", &url, &clone.to_string_lossy()])?;
    }
    git(clone, &["remote", "set-url", "origin", &url])?;
    git(clone, &["fetch", "-q", "--prune", "--prune-tags", "origin"])?;
    let remote = format!("refs/remotes/origin/{}", br);
    if git(clone, &["rev-parse", "-q", "--verify", &remote]).is_ok() {
        git(clone, &["checkout", "-q", "-f", "-B", &br, &remote])?;
        git(clone, &["reset", "-q", "--hard", &remote])?;
    } else {
        git(clone, &["symbolic-ref", "HEAD", &format!("refs/heads/{}", br)])?;
        let _ = git(clone, &["update-ref", "-d", &format!("refs/heads/{}", br)]);
        git(clone, &["read-tree", "--empty"])?;
    }
    git(clone, &["clean", "-qfdx"])?;
    Ok(())
}
fn released_source(clone: &Path) -> Option<String> {
    git(clone, &["log", "-1", "--format=%B"]).ok()?.lines().find_map(|l| l.strip_prefix("Source-Commit: ").map(|s| s.trim().to_string()))
}
fn last_tag(clone: &Path, prefix: &str) -> Option<String> {
    git(clone, &["tag", "-l", &format!("{}*", prefix), "--sort=-v:refname"]).ok()?.lines().next().map(String::from)
}

fn kill(pid: u32) {
    let pid = pid.to_string();
    let _ = if cfg!(windows) { Command::new("taskkill").args(["/F", "/T", "/PID", &pid]).output() } else { Command::new("kill").args(["-9", &pid]).output() };
}

/// Each `oss.tests` command runs in the staged export with a throwaway HOME/KIWI_HOME (no real state, tokens or git identity),
/// shared cargo caches, and a timeout. `{kiwi}` = this binary, `{tmp}` = the throwaway dir (both also in a test's `env`).
fn run_tests(c: &Cfg, dir: &Path) -> Vec<Value> {
    let base = home().join("oss");
    let kiwi = std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| "kiwi".into());
    let real = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_default();
    let cargo_home = std::env::var("CARGO_HOME").unwrap_or_else(|_| format!("{}/.cargo", real));
    let rustup_home = std::env::var("RUSTUP_HOME").unwrap_or_else(|_| format!("{}/.rustup", real));
    let limit = std::time::Duration::from_secs(int_at(&c.v, "/test_timeout_seconds", 1800) as u64);
    let pass = ["PATH", "LANG", "LC_ALL", "TZ", "TERM", "TMPDIR", "TEMP", "TMP", "SystemRoot", "SYSTEMROOT", "windir", "ComSpec", "PATHEXT"];
    let mut out = vec![];
    for (i, t) in arr_at(&c.v, "/tests").iter().enumerate() {
        let name = text_at(t, "/name");
        let tmp = base.join("tmp").join(format!("t{}", i));
        let _ = std::fs::remove_dir_all(&tmp); let _ = std::fs::create_dir_all(tmp.join("kiwi"));
        let _ = std::fs::write(tmp.join(".gitconfig"), format!("[user]\n\tname = {}\n\temail = {}\n[init]\n\tdefaultBranch = main\n", text_at(&c.v, "/git/name"), text_at(&c.v, "/git/email")));
        let sub = |x: &str| x.replace("{kiwi}", &kiwi).replace("{tmp}", &tmp.to_string_lossy());
        let cmd: Vec<String> = list_at(t, "/cmd").iter().map(|x| sub(x)).collect();
        let extra: Vec<(String, String)> = t.get("env").and_then(Value::as_object).into_iter().flatten().filter_map(|(k, v)| Some((k.clone(), sub(v.as_str()?)))).collect();
        let started = std::time::Instant::now();
        let mut command = Command::new(cmd.first().map_or("", String::as_str));
        command.args(cmd.iter().skip(1)).current_dir(dir.join(text_at(t, "/cwd"))).env_clear()
            .envs(pass.iter().filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v))))
            .env("HOME", &tmp).env("USERPROFILE", &tmp).env("KIWI_HOME", tmp.join("kiwi")).env("KIWI_LIVE_STATE_DIR", tmp.join("state"))
            .env("CARGO_HOME", &cargo_home).env("RUSTUP_HOME", &rustup_home).env("CARGO_TARGET_DIR", base.join("target")).env("PYTHONDONTWRITEBYTECODE", "1").envs(extra)
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let tail = |o: &[u8]| { let s = String::from_utf8_lossy(o); let l: Vec<&str> = s.lines().collect(); l[l.len().saturating_sub(15)..].join("\n") };
        let (passed, detail) = match command.spawn() {
            Err(x) => (false, x.to_string()),
            Ok(child) => {
                let pid = child.id();
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || { let _ = tx.send(child.wait_with_output()); });
                match rx.recv_timeout(limit) {
                    Ok(Ok(o)) => (o.status.success(), format!("{}\n{}", tail(&o.stdout), tail(&o.stderr)).trim().to_string()),
                    Ok(Err(x)) => (false, x.to_string()),
                    Err(_) => { kill(pid); (false, format!("timeout after {}s", limit.as_secs())) }
                }
            }
        };
        out.push(json!({"name": name, "passed": passed, "seconds": started.elapsed().as_secs(), "tail": detail}));
    }
    let _ = std::fs::remove_dir_all(base.join("tmp"));
    out
}

fn failed(st: &mut Value, dry: bool, head: &str, stage: &str, detail: Value, code: i32) -> Res<(Value, i32)> {
    let mut out = json!({"published": false, "reason": format!("{}_failed", stage), "head": head});
    if let (Some(o), Some(d)) = (out.as_object_mut(), detail.as_object()) { for (k, v) in d { o.insert(k.clone(), v.clone()); } }
    if !dry { st["last_failure"] = json!({"stage": stage, "head": head, "tree": detail.get("tree"), "at": iso(now())}); save_state(st)?; }
    Ok((out, code))
}

/// Handoff for the marketing bot: real user-visible changes, real screens to record, video spec, copy limits, and the
/// approval rule (an X post needs the owner's approval; nothing is posted automatically).
#[allow(clippy::too_many_arguments)]
fn released(c: &Cfg, version: &str, tag: &str, prev: Option<&str>, oss_sha: &str, changes: &[Value], first: bool, tests: &[Value], files: usize, head: &str, commits: usize) -> Value {
    let (m, url) = (c.v.get("marketing").cloned().unwrap_or(json!({})), text_at(&c.v, "/web_url"));
    let surfaces: BTreeSet<&str> = changes.iter().filter_map(|x| x["surface"].as_str()).collect();
    let demo: Vec<Value> = arr_at(&m, "/demo").into_iter().filter(|d| d["surface"].as_str().map_or(false, |s| surfaces.contains(s))).collect();
    let highlights: Vec<Value> = changes.iter().filter(|x| x["kind"] == "feature").chain(changes.iter().filter(|x| x["kind"] != "feature"))
        .take(int_at(&m, "/highlights", 3) as usize).map(|x| x["title"].clone()).collect();
    json!({
        "schema": 1, "repo": text_at(&c.v, "/repo"), "repo_url": url, "version": version, "tag": tag, "previous": prev, "oss_sha": oss_sha,
        "tree_url": format!("{}/tree/{}", url, tag), "compare_url": prev.map(|p| format!("{}/compare/{}...{}", url, p, tag)),
        "released_at": iso(now()), "first_release": first, "user_visible": !changes.is_empty(), "license": c.v.get("license"),
        "changes": changes, "highlights": highlights, "demo": demo, "video": m.get("video"), "copy": m.get("copy"), "approval": m.get("approval"),
        "scan": {"passed": true, "files": files, "findings": 0},
        "tests": tests.iter().map(|t| json!({"name": t["name"], "passed": t["passed"], "seconds": t["seconds"]})).collect::<Vec<_>>(),
        "source": {"kiwi_sha": head, "commits": commits},
    })
}

fn release(a: &Args, force: bool) -> Res<(Value, i32)> {
    let repo = PathBuf::from(a.req("repo")?);
    let head = head_or(&repo, a)?;
    let c = load(&repo, &head)?;
    if c.v.get("enabled").and_then(Value::as_bool) != Some(true) { return Ok((json!({"published": false, "reason": "disabled"}), 0)); }
    let dry = a.flag("dry-run");
    let dir = home().join("oss"); std::fs::create_dir_all(&dir).map_err(e)?;
    let Some(_lock) = Lock::take(dir.join("oss.lock"))? else { return Ok((json!({"published": false, "reason": "busy"}), 0)) };
    let mut st = load_state();
    // The outbox goes first, so queued events are delivered whatever this run decides.
    let delivered = if dry { vec![] } else { let d = drain(&c, &mut st); save_state(&st)?; d };
    let (mut out, code) = publish(&repo, &head, &c, &mut st, &dir, dry, force)?;
    if !delivered.is_empty() { out["delivered"] = json!(delivered); }
    Ok((out, code))
}

/// One batch: window check, kiwi.improved, then export → scan → tests → commit + tag → push → oss.released.
fn publish(repo: &Path, head: &str, c: &Cfg, st: &mut Value, dir: &Path, dry: bool, force: bool) -> Res<(Value, i32)> {
    let clone = home().join(text_at(&c.v, "/clone_dir"));
    let prefix = text_at(&c.v, "/version/tag_prefix");
    // State first (no network); a fresh machine recovers base and version from the published repo.
    let (base, last) = match (st.pointer("/source_sha").and_then(Value::as_str), st.pointer("/last_release/tag").and_then(Value::as_str)) {
        (Some(b), Some(t)) => (Some(b.to_string()), Some(t.to_string())),
        _ => { sync(c, &clone)?; (released_source(&clone), last_tag(&clone, &prefix)) }
    };
    let base = base.filter(|b| git(repo, &["merge-base", "--is-ancestor", b, head]).is_ok());
    let all = digest(repo, c, base.as_deref(), head)?;
    let pending: Vec<&Change> = all.iter().filter(|x| !x.files.is_empty()).collect();
    if pending.is_empty() && last.is_some() { return Ok((json!({"published": false, "reason": "no_changes", "head": head}), 0)); }
    let window = int_at(&c.v, "/improved/window_minutes", 1440);
    let oldest = pending.iter().map(|x| x.at).min().unwrap_or_else(now);
    if !force && now() < oldest + window * 60 {
        return Ok((json!({"published": false, "reason": "batching", "pending": pending.len(), "due_at": iso(oldest + window * 60)}), 0));
    }
    let mut events = vec![];
    // Once per batch (keyed by its base), so a blocked release does not re-announce on every later commit.
    if !dry && st.pointer("/improved/since") != Some(&json!(base)) {
        let data = json!({"schema": 1, "repo": text_at(&c.v, "/repo"), "since": base, "head": head, "window_minutes": window, "oldest_at": iso(oldest),
            "closed_by": if force { "flush" } else { "window" }, "count": pending.len(),
            "commits": pending.iter().take(50).map(|x| json!({"sha": x.sha, "at": iso(x.at), "title": title(c, &x.subject), "paths": x.files.iter().take(10).collect::<Vec<_>>()})).collect::<Vec<_>>()});
        events.push(send(c, st, &text_at(&c.v, "/improved/event"), data));
        st["improved"] = json!({"since": base, "head": head, "at": iso(now())});
        save_state(st)?;
    }

    let ex = build(repo, head, c)?;
    let findings = scan_files(c, &ex.files);
    if !findings.is_empty() { return failed(st, dry, head, "scan", json!({"scan": report(&findings), "events": events}), 1); }
    sync(c, &clone)?;
    for f in git(&clone, &["ls-files", "-z"])?.split('\0').filter(|f| !f.is_empty() && *f != "CHANGELOG.md") { let _ = std::fs::remove_file(clone.join(f)); }
    write_files(&clone, &ex)?;
    git(&clone, &["add", "-A"])?;
    // Modes from the source tree, also where the clone's filesystem has no exec bit (core.filemode=false).
    let (x, plain): (Vec<&String>, Vec<&String>) = ex.files.keys().partition(|p| ex.exec.contains(*p));
    for (flag, paths) in [("--chmod=+x", x), ("--chmod=-x", plain)].into_iter().filter(|(_, p)| !p.is_empty()) {
        git(&clone, &[&["update-index", flag, "--"][..], &paths.iter().map(|p| p.as_str()).collect::<Vec<_>>()].concat())?;
    }
    let changed: BTreeSet<String> = git(&clone, &["diff", "--cached", "--name-only", "-z"])?.split('\0').filter(|f| !f.is_empty()).map(String::from).collect();
    if changed.is_empty() {
        if !dry { st["source_sha"] = json!(head); save_state(st)?; }
        return Ok((json!({"published": false, "reason": "unchanged", "head": head, "events": events}), 0));
    }
    let tree = git(&clone, &["write-tree"])?;
    if st.pointer("/last_failure/tree").and_then(Value::as_str) == Some(tree.as_str()) {
        let _ = sync(c, &clone);
        return Ok((json!({"published": false, "reason": "tests_failed_same_tree", "tree": tree, "events": events}), 1));
    }
    let stage = dir.join("stage");
    let _ = std::fs::remove_dir_all(&stage);
    write_files(&stage, &ex)?;
    let tests = run_tests(c, &stage);
    let _ = std::fs::remove_dir_all(&stage);
    if tests.iter().any(|t| t["passed"] != true) { let _ = sync(c, &clone); return failed(st, dry, head, "tests", json!({"tree": tree, "tests": tests, "events": events}), 1); }

    let first = last.is_none();
    let changes = if first { arr_at(&c.v, "/changelog/first_release") } else { changelog(c, &pending, &changed) };
    let version = match &last { None => text_at(&c.v, "/version/first"), Some(t) => bump(t.strip_prefix(prefix.as_str()).unwrap_or(t), changes.iter().any(|x| x["kind"] == "feature")) };
    let tag = format!("{}{}", prefix, version);
    let notes = bullets(c, &changes);
    let old = std::fs::read_to_string(clone.join("CHANGELOG.md")).unwrap_or_default();
    let rest = old.strip_prefix("# Changelog").unwrap_or(&old).trim_start_matches('\n');
    std::fs::write(clone.join("CHANGELOG.md"), format!("# Changelog\n\n## {} — {}\n\n{}\n{}", tag, &iso(now())[..10], notes, rest)).map_err(e)?;
    git(&clone, &["add", "CHANGELOG.md"])?;
    let (name, email) = (text_at(&c.v, "/git/name"), text_at(&c.v, "/git/email"));
    let message = format!("Release {}\n\n{}\nSource-Commit: {}\n", tag, notes, head);
    let mut tracked = Files::new();
    for f in git(&clone, &["ls-files", "-z"])?.split('\0').filter(|f| !f.is_empty()) { tracked.insert(f.to_string(), std::fs::read(clone.join(f)).map_err(e)?); }
    let mut fin = scan_files(c, &tracked);
    scan_text(c, "<commit>", &format!("{}\n{} <{}>\n{}", message, name, email, tag), &mut fin);
    if !fin.is_empty() { let _ = sync(c, &clone); return failed(st, dry, head, "final_scan", json!({"scan": report(&fin), "events": events}), 1); }
    let commits = pending.len();
    if dry {
        let _ = sync(c, &clone);
        let preview = released(c, &version, &tag, last.as_deref(), "", &changes, first, &tests, tracked.len(), head, commits);
        return Ok((json!({"published": false, "dry_run": true, "version": version, "tag": tag, "changed": changed.len(), "excluded": ex.excluded, "scan": report(&fin), "tests": tests, "payload": preview}), 0));
    }

    let id = [("GIT_AUTHOR_NAME", name.as_str()), ("GIT_AUTHOR_EMAIL", email.as_str()), ("GIT_COMMITTER_NAME", name.as_str()), ("GIT_COMMITTER_EMAIL", email.as_str())];
    git_out(&clone, &["commit", "-q", "--no-verify", "-m", &message], &id)?;
    git_out(&clone, &["tag", "-a", &tag, "-m", &format!("{}\n\n{}", tag, notes)], &id)?;
    let br = text_at(&c.v, "/branch");
    git(&clone, &["push", "-q", "--atomic", "origin", &format!("HEAD:refs/heads/{}", br), &format!("refs/tags/{}", tag)])?;
    let oss_sha = git(&clone, &["rev-parse", "HEAD"])?;
    let remote = git(&clone, &["ls-remote", "origin", &format!("refs/heads/{}", br)])?;
    if remote.split_whitespace().next() != Some(oss_sha.as_str()) { return failed(st, false, head, "push_confirm", json!({"oss_sha": oss_sha}), 1); }
    st["last_release"] = json!({"version": version, "tag": tag, "oss_sha": oss_sha, "source_sha": head, "at": iso(now())});
    st["source_sha"] = json!(head);
    st["last_failure"] = Value::Null;
    save_state(st)?;
    let payload = released(c, &version, &tag, last.as_deref(), &oss_sha, &changes, first, &tests, tracked.len(), head, commits);
    let mut pf = vec![];
    scan_text(c, "<payload>", &serde_json::to_string_pretty(&payload).map_err(e)?, &mut pf);
    events.push(if pf.is_empty() { send(c, st, &text_at(&c.v, "/released/event"), payload.clone()) } else { json!({"event": "skipped", "scan": report(&pf)}) });
    save_state(st)?;
    Ok((json!({"published": true, "version": version, "tag": tag, "oss_sha": oss_sha, "changed": changed.len(), "events": events, "payload": payload}), 0))
}

fn head_or(repo: &Path, a: &Args) -> Res<String> { match a.get("sha") { Some(s) => git(repo, &["rev-parse", &s]), None => git(repo, &["rev-parse", "HEAD"]) } }

fn export(a: &Args) -> Res<(Value, i32)> {
    let (repo, out) = (PathBuf::from(a.req("repo")?), PathBuf::from(a.req("out")?));
    let sha = head_or(&repo, a)?;
    let c = load(&repo, &sha)?;
    if out.exists() && std::fs::read_dir(&out).map_err(e)?.next().is_some() { return Err(format!("{} is not empty", out.display())); }
    let ex = build(&repo, &sha, &c)?;
    let findings = scan_files(&c, &ex.files);
    write_files(&out, &ex)?;
    let bytes: usize = ex.files.values().map(Vec::len).sum();
    Ok((json!({"sha": sha, "out": out, "files": ex.files.len(), "bytes": bytes, "excluded": ex.excluded, "scan": report(&findings)}), if findings.is_empty() { 0 } else { 1 }))
}

fn scan_dir(a: &Args) -> Res<(Value, i32)> {
    let repo = PathBuf::from(a.req("repo")?);
    let c = load(&repo, &head_or(&repo, a)?)?;
    let mut files = Files::new();
    read_dir_files(Path::new(&a.req("dir")?), "", &mut files)?;
    let f = scan_files(&c, &files);
    Ok((json!({"files": files.len(), "scan": report(&f)}), if f.is_empty() { 0 } else { 1 }))
}

fn status(a: &Args) -> Res<(Value, i32)> {
    let st = load_state();
    let outbox: Vec<Value> = st["outbox"].as_array().into_iter().flatten().map(|x| json!({"event": x["event"], "error": x["error"], "queued_at": x["queued_at"]})).collect();
    let mut v = json!({"state": state_path(), "source_sha": st["source_sha"], "last_release": st["last_release"], "last_failure": st["last_failure"], "improved": st["improved"], "outbox": outbox});
    if let Some(r) = a.get("repo") {
        let r = PathBuf::from(r);
        let c = load(&r, &head_or(&r, a)?)?;
        v["config"] = json!({"enabled": c.v.get("enabled"), "repo": c.v.get("repo"), "window_minutes": int_at(&c.v, "/improved/window_minutes", 1440), "clone": home().join(text_at(&c.v, "/clone_dir"))});
    }
    Ok((v, 0))
}

pub fn oss(cmd: &str, a: &Args) -> Res<(Value, i32)> {
    match cmd {
        "export" => export(a),
        "scan" => scan_dir(a),
        "tick" => release(a, false),
        "flush" => release(a, true),
        "status" => status(a),
        _ => Err("usage: kiwi oss export|scan|tick|flush|status --repo CLONE".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::{bump, hit, iso, norm};

    #[test]
    fn path_patterns() {
        assert!(hit("accounts/a.md", "accounts/") && !hit("x/accounts/a.md", "accounts/"));
        assert!(hit("a/target/b", "**/target/") && !hit("a/target", "**/target/"));
        assert!(hit("a/b.pem", "**/*.pem") && hit(".env", "**/.env") && hit("a/.env", "**/.env") && !hit("a/.envrc", "**/.env"));
        assert!(hit("rules/x.md", "rules/x.md") && !hit("rules/x.mdx", "rules/x.md"));
    }

    #[test]
    fn helpers() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(1_793_000_000), "2026-10-26T07:33:20Z");
        assert_eq!((bump("0.1.0", true), bump("0.1.3", false)), ("0.2.0".to_string(), "0.1.4".to_string()));
        assert_eq!(norm("a/b", "../c.md"), Some("a/c.md".into()));
        assert_eq!(norm("", "../x"), None);
    }
}
