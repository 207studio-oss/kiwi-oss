use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;

pub type Res<T> = Result<T, String>;

pub fn e<E: std::fmt::Display>(x: E) -> String { x.to_string() }

pub fn sha(s: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(s);
    h.finalize().iter().map(|b| format!("{:02x}", b)).collect()
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// KIWI_HOME holds ops.db (state, back up) and kiwi.db (compiled, regenerable). Never inside the repo.
pub fn home() -> PathBuf {
    std::env::var("KIWI_HOME").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".kiwi")
    })
}

/// `--key value` / `--flag` parser. Repeated keys accumulate.
pub struct Args { pub pos: Vec<String>, pub kv: HashMap<String, Vec<String>> }

impl Args {
    pub fn parse(v: &[String]) -> Args {
        let mut pos = vec![]; let mut kv: HashMap<String, Vec<String>> = HashMap::new();
        let mut i = 0;
        while i < v.len() {
            if let Some(k) = v[i].strip_prefix("--") {
                if i + 1 < v.len() && !v[i + 1].starts_with("--") { kv.entry(k.into()).or_default().push(v[i + 1].clone()); i += 2; }
                else { kv.entry(k.into()).or_default().push("true".into()); i += 1; }
            } else { pos.push(v[i].clone()); i += 1; }
        }
        Args { pos, kv }
    }
    pub fn get(&self, k: &str) -> Option<String> { self.kv.get(k).and_then(|v| v.last().cloned()) }
    pub fn req(&self, k: &str) -> Res<String> { self.get(k).ok_or(format!("missing --{}", k)) }
    pub fn list(&self, k: &str) -> Vec<String> {
        self.kv.get(k).map(|v| v.iter().flat_map(|s| s.split(',').map(|x| x.trim().to_string())).filter(|s| !s.is_empty()).collect()).unwrap_or_default()
    }
    pub fn flag(&self, k: &str) -> bool { self.kv.contains_key(k) }
}

/// Paths excluded from sources/patches: only git internals and build output.
/// Sensitive material (accounts/, secrets, .env, *.key, *.pem) is intentionally allowed: Kiwi exists so bots can read it
/// (오너 2026-10-23 17:54). The Rust kiwi-sprouts port must match.
pub fn denied(rel: &str) -> bool {
    let r = rel.trim_start_matches("./").to_lowercase();
    r.split('/').any(|p| matches!(p, ".git" | "target" | "node_modules"))
}

#[cfg(test)]
mod tests {
    use super::denied;

    #[test]
    fn approved_sensitive_paths_are_allowed() {
        for path in ["accounts/k.md","secrets/k.md",".env",".env.local","k.key","k.pem","token.md","secret.md","k.p8"] {
            assert!(!denied(path),"{path}");
        }
        for path in [".git/config","x/target/a","node_modules/a","X/.GIT/config"] {
            assert!(denied(path),"{path}");
        }
    }
}
