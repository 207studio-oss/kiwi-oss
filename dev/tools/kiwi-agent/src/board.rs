use crate::finding::{unconfirmed, Result};
use crate::source::{reject_sensitive_path, Repository};
use std::path::Path;

const BOARD: &str = "bots/active/";
const KIWI: &str = "github.com/207studio-founder/kiwi";
const RENAMED: &[(&str, &str)] = &[
    ("github.com/207studio-founder/kiwi", KIWI),
    ("github.com/207studio-founder/kiwi-private", KIWI),
];

pub struct ClaimFile {
    pub path: String,
    pub blob: String,
    fields: Option<toml::Table>,
}

impl ClaimFile {
    pub fn field(&self, key: &str) -> Option<&str> {
        self.fields.as_ref()?.get(key)?.as_str()
    }

    pub fn expires_at(&self) -> Option<u64> {
        crate::source_time::timestamp(self.field("expires")?).ok()
    }
}

pub fn snapshot(kiwi: &Repository) -> Result<Vec<ClaimFile>> {
    if let Some(path) = kiwi.changed(&[BOARD])?.into_iter().find(|p| active_file(p)) {
        return Err(unconfirmed("source_not_committed", path));
    }
    let mut claims = Vec::new();
    for entry in kiwi.entries(BOARD)? {
        if !active_file(&entry.path) {
            continue;
        }
        reject_sensitive_path(Path::new(&entry.path))?;
        if !entry.regular() {
            return Err(unconfirmed("source_not_regular", entry.path));
        }
        let body = kiwi.git(&["cat-file", "blob", &entry.blob])?;
        claims.push(ClaimFile {
            fields: std::str::from_utf8(&body)
                .ok()
                .and_then(|text| toml::from_str(text).ok()),
            path: entry.path,
            blob: entry.blob,
        });
    }
    Ok(claims)
}

fn current_name(repo: &str) -> &str {
    RENAMED
        .iter()
        .find(|&&(old, _)| old == repo)
        .map_or(repo, |&(_, new)| new)
}

pub fn same_repository(a: &str, b: &str) -> bool {
    current_name(a) == current_name(b)
}

pub fn is_kiwi(remote: &str) -> bool {
    same_repository(remote, KIWI)
}

pub fn canonical_remote(value: &str) -> Result<String> {
    let lower = value
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .to_ascii_lowercase();
    let plain = lower
        .strip_prefix("https://github.com/")
        .or_else(|| lower.strip_prefix("ssh://git@github.com/"))
        .or_else(|| lower.strip_prefix("git@github.com:"))
        .or_else(|| lower.strip_prefix("github.com/"))
        .unwrap_or(&lower);
    let parts: Vec<_> = plain.split('/').collect();
    if parts.len() != 2
        || parts.iter().any(|p| {
            p.is_empty()
                || !p
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
                || *p == "."
                || *p == ".."
        })
    {
        return Err(unconfirmed(
            "unsupported_repository_alias",
            "only exact owner/repository and GitHub remote aliases are supported",
        ));
    }
    Ok(format!("github.com/{plain}"))
}

pub fn uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

fn active_file(path: &str) -> bool {
    path.starts_with(BOARD) && path.ends_with(".toml") && path.split('/').count() == 3
}
