use crate::board::canonical_remote;
use crate::finding::{unconfirmed, Finding, Result};
use crate::git_command::{self, Failure};
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::path::{Component, Path, PathBuf};

pub struct Repository {
    pub root: PathBuf,
    pub head: String,
}

#[derive(Serialize)]
pub struct Upstream {
    pub name: String,
    pub commit: String,
}

pub struct Entry {
    pub path: String,
    pub mode: String,
    pub blob: String,
}

impl Repository {
    pub fn open(path: &Path) -> Result<Self> {
        reject_sensitive_path(path)?;
        reject_symlinks(path)?;
        let root = fs::canonicalize(path)
            .map_err(|_| unconfirmed("repository_unreadable", path.display().to_string()))?;
        reject_sensitive_path(&root)?;
        let mut repo = Self {
            root,
            head: String::new(),
        };
        if repo.text(&["rev-parse", "--is-inside-work-tree", "--show-prefix"])? != "true\n\n" {
            return Err(unconfirmed(
                "repository_root_required",
                repo.root.display().to_string(),
            ));
        }
        repo.head = repo
            .text(&["rev-parse", "--verify", "HEAD^{commit}"])?
            .trim()
            .into();
        if !matches!(repo.head.len(), 40 | 64) || !repo.head.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err(unconfirmed("invalid_head", "Git HEAD"));
        }
        Ok(repo)
    }

    pub fn git(&self, args: &[&str]) -> Result<Vec<u8>> {
        let output = git_command::output(&self.root, args).map_err(|f| failure(f, args))?;
        if !output.status.success() {
            return Err(unconfirmed(
                "git_read_failed",
                args.first().copied().unwrap_or("git"),
            ));
        }
        Ok(output.stdout)
    }

    pub fn text(&self, args: &[&str]) -> Result<String> {
        String::from_utf8(self.git(args)?).map_err(|_| unconfirmed("git_non_utf8", "Git metadata"))
    }

    fn resolved(&self, args: &[&str]) -> Result<Option<String>> {
        let output = git_command::output(&self.root, args).map_err(|f| failure(f, args))?;
        if !output.status.success() {
            return Ok(None);
        }
        let text = String::from_utf8(output.stdout)
            .map_err(|_| unconfirmed("git_non_utf8", "Git metadata"))?;
        Ok(Some(text.trim().to_string()).filter(|t| !t.is_empty()))
    }

    pub fn upstream(&self) -> Result<Option<Upstream>> {
        let Some(name) = self
            .resolved(&["rev-parse", "--symbolic-full-name", "@{u}"])?
            .filter(|n| n.starts_with("refs/remotes/"))
        else {
            return Ok(None);
        };
        let commit = format!("{name}^{{commit}}");
        let Some(commit) = self.resolved(&["rev-parse", "--verify", "--quiet", &commit])? else {
            return Ok(None);
        };
        Ok(Some(Upstream { name, commit }))
    }

    pub fn remote(&self) -> Result<String> {
        let tracked = format!("branch.{}.remote", self.branch()?);
        let name = match self.resolved(&["config", "--get", &tracked])? {
            Some(name) if name != "." => name,
            _ => match self.text(&["remote"])?.lines().collect::<Vec<_>>()[..] {
                [only] => only.to_string(),
                _ => {
                    return Err(unconfirmed(
                        "remote_unresolved",
                        "an upstream remote or exactly one configured remote is required",
                    ))
                }
            },
        };
        canonical_remote(
            self.text(&["config", "--get", &format!("remote.{name}.url")])?
                .trim(),
        )
    }

    pub fn branch(&self) -> Result<String> {
        Ok(self
            .text(&["rev-parse", "--abbrev-ref", "HEAD"])?
            .trim()
            .into())
    }

    pub fn changed(&self, pathspec: &[impl AsRef<str>]) -> Result<Vec<String>> {
        let literal: Vec<_> = pathspec
            .iter()
            .map(|p| format!(":(literal){}", p.as_ref()))
            .collect();
        let mut diff = vec![
            "diff",
            "--no-ext-diff",
            "--no-renames",
            "--ignore-submodules=none",
            "--name-only",
            "-z",
            self.head.as_str(),
            "--",
        ];
        diff.extend(literal.iter().map(String::as_str));
        let mut untracked = vec!["ls-files", "--others", "--exclude-standard", "-z", "--"];
        untracked.extend(literal.iter().map(String::as_str));
        let mut paths = names(&self.git(&diff)?)?;
        paths.extend(names(&self.git(&untracked)?)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }

    pub fn entries(&self, path: &str) -> Result<Vec<Entry>> {
        let literal = format!(":(literal){path}");
        let tree = self.git(&["ls-tree", "-rz", "--full-tree", &self.head, "--", &literal])?;
        let mut entries = Vec::new();
        for row in tree.split(|c| *c == 0).filter(|v| !v.is_empty()) {
            let row =
                std::str::from_utf8(row).map_err(|_| unconfirmed("invalid_tree", "Git tree"))?;
            let (meta, name) = row
                .split_once('\t')
                .ok_or_else(|| unconfirmed("invalid_tree", "Git tree"))?;
            let fields: Vec<_> = meta.split(' ').collect();
            if fields.len() == 3 && fields[1] == "blob" {
                entries.push(Entry {
                    path: name.into(),
                    mode: fields[0].into(),
                    blob: fields[2].into(),
                });
            }
        }
        Ok(entries)
    }
}

impl Entry {
    pub fn regular(&self) -> bool {
        self.mode == "100644" || self.mode == "100755"
    }
}

fn failure(failure: Failure, args: &[&str]) -> Finding {
    match failure {
        Failure::Unavailable => unconfirmed("git_unavailable", "git"),
        Failure::TooLarge => unconfirmed("git_read_failed", args.first().copied().unwrap_or("git")),
    }
}

pub fn scope(path: &str) -> Result<String> {
    let clean = path.strip_suffix('/').unwrap_or(path);
    if clean.is_empty()
        || clean.len() > 1024
        || clean.contains('\\')
        || clean.chars().any(|c| "*?[]{}".contains(c))
        || clean.chars().any(char::is_control)
        || Path::new(clean)
            .components()
            .any(|p| !matches!(p, Component::Normal(_)))
        || clean
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(unconfirmed(
            "invalid_scope",
            "relative path components required; omit --path for whole repository",
        ));
    }
    Ok(clean.into())
}

pub fn reject_sensitive_path(path: &Path) -> Result<()> {
    for part in path.components().filter_map(|p| {
        if let Component::Normal(s) = p {
            s.to_str()
        } else {
            None
        }
    }) {
        if crate::sensitive_paths::excluded_component(part) {
            return Err(unconfirmed(
                "source_not_allowed",
                "credential path excluded",
            ));
        }
    }
    Ok(())
}

pub fn names(bytes: &[u8]) -> Result<Vec<String>> {
    bytes
        .split(|c| *c == 0)
        .filter(|n| !n.is_empty())
        .map(|n| {
            String::from_utf8(n.to_vec()).map_err(|_| unconfirmed("non_utf8_path", "Git path"))
        })
        .collect()
}

fn reject_symlinks(path: &Path) -> Result<()> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|_| unconfirmed("current_directory_unreadable", "working directory"))?
            .join(path)
    };
    let mut current = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::ParentDir => {
                current.pop();
            }
            Component::CurDir => {}
            other => current.push(other),
        }
        let metadata = fs::symlink_metadata(&current)
            .map_err(|_| unconfirmed("source_unreadable", path.display().to_string()))?;
        if metadata.file_type().is_symlink() {
            return Err(unconfirmed("source_symlink", path.display().to_string()));
        }
    }
    Ok(())
}

pub fn receipt_file(path: &Path) -> Result<File> {
    reject_sensitive_path(path)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    reject_symlinks(parent)?;
    let parent = fs::canonicalize(parent)
        .map_err(|_| unconfirmed("receipt_parent_unreadable", "receipt"))?;
    let name = path
        .file_name()
        .ok_or_else(|| unconfirmed("invalid_receipt_path", "receipt"))?;
    let target = parent.join(name);
    reject_sensitive_path(&target)?;
    if fs::symlink_metadata(&target).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(unconfirmed("source_symlink", "receipt"));
    }
    let toplevel = |cwd: &Path| -> Result<Option<PathBuf>> {
        let probe = git_command::output(cwd, &["rev-parse", "--show-toplevel"])
            .map_err(|_| unconfirmed("git_unavailable", "receipt tracking requires git"))?;
        if probe.status.success() {
            let top = String::from_utf8(probe.stdout)
                .map_err(|_| unconfirmed("git_non_utf8", "receipt"))?;
            return fs::canonicalize(top.trim())
                .map(Some)
                .map_err(|_| unconfirmed("receipt_tracking_unconfirmed", "receipt"));
        }
        if probe.status.code() == Some(128)
            && probe.stdout.is_empty()
            && String::from_utf8_lossy(&probe.stderr).contains("not a git repository")
        {
            return Ok(None);
        }
        Err(unconfirmed("receipt_tracking_unconfirmed", "receipt"))
    };
    let mut roots = BTreeSet::new();
    for ancestor in parent.ancestors() {
        if fs::symlink_metadata(ancestor.join(".git")).is_ok() {
            roots.insert(
                toplevel(ancestor)?
                    .ok_or_else(|| unconfirmed("receipt_tracking_unconfirmed", "receipt"))?,
            );
        }
    }
    match toplevel(&parent)? {
        Some(root) => {
            roots.insert(root);
        }
        None if !roots.is_empty() => {
            return Err(unconfirmed("receipt_tracking_unconfirmed", "receipt"));
        }
        None => {}
    }
    for root in &roots {
        let relative = target
            .strip_prefix(root)
            .map_err(|_| unconfirmed("invalid_receipt_path", "receipt"))?;
        let relative = relative
            .to_str()
            .ok_or_else(|| unconfirmed("invalid_receipt_path", "receipt"))?;
        let literal = format!(":(literal,icase){relative}");
        let tracked = git_command::output(
            root,
            &[
                "ls-files",
                "--error-unmatch",
                "--with-tree=HEAD",
                "--",
                &literal,
            ],
        )
        .map_err(|_| unconfirmed("receipt_tracking_unconfirmed", "receipt"))?;
        match tracked.status.code() {
            Some(0) => {
                return Err(unconfirmed(
                    "receipt_tracked_source",
                    "receipt cannot replace any tracked Git source, including a deleted file or a case alias",
                ))
            }
            Some(1) => {}
            _ => return Err(unconfirmed("receipt_tracking_unconfirmed", "receipt")),
        }
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(target)
        .map_err(|_| unconfirmed("receipt_must_be_new_file", "receipt"))
}
