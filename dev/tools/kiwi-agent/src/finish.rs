use crate::board::{self, canonical_remote, uuid, ClaimFile};
use crate::finding::{finding, unconfirmed, Finding, Result, State};
use crate::source::{receipt_file, reject_sensitive_path, scope, Repository, Upstream};
use serde::Serialize;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const USAGE: &str = "kiwi-agent finish --kiwi-repo PATH --project-repo PATH --agent-id UUID --branch NAME --receipt NEW_FILE [--path REL]* [--log logs/REL.md] [--keep-claim]";
const MAX_LOG: u64 = 1024 * 1024;

#[derive(Default)]
struct FinishOptions {
    kiwi: PathBuf,
    project: PathBuf,
    agent: String,
    branch: String,
    receipt: PathBuf,
    paths: Vec<String>,
    log: Option<String>,
    keep_claim: bool,
}

impl FinishOptions {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self> {
        let mut args = args.peekable();
        if args.peek().is_none() {
            return Err(unconfirmed("usage", USAGE));
        }
        let mut options = Self::default();
        while let Some(key) = args.next() {
            if key == "--keep-claim" && !options.keep_claim {
                options.keep_claim = true;
                continue;
            }
            let value = args
                .next()
                .ok_or_else(|| unconfirmed("missing_argument", &key))?;
            match key.as_str() {
                "--kiwi-repo" if options.kiwi.as_os_str().is_empty() => options.kiwi = value.into(),
                "--project-repo" if options.project.as_os_str().is_empty() => {
                    options.project = value.into()
                }
                "--agent-id" if options.agent.is_empty() => options.agent = value,
                "--branch" if options.branch.is_empty() => options.branch = value,
                "--receipt" if options.receipt.as_os_str().is_empty() => {
                    options.receipt = value.into()
                }
                "--path" => options.paths.push(scope(&value)?),
                "--log" if options.log.is_none() => options.log = Some(log_path(&value)?),
                _ => return Err(unconfirmed("unknown_or_duplicate_argument", key)),
            }
        }
        if options.kiwi.as_os_str().is_empty()
            || options.project.as_os_str().is_empty()
            || options.receipt.as_os_str().is_empty()
            || !uuid(&options.agent)
            || options.branch.is_empty()
            || options
                .branch
                .chars()
                .any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(unconfirmed("required_argument", USAGE));
        }
        options.paths.sort();
        options.paths.dedup();
        Ok(options)
    }
}

fn log_path(value: &str) -> Result<String> {
    let path = scope(value)
        .ok()
        .filter(|p| p.starts_with("logs/") && p.ends_with(".md"))
        .ok_or_else(|| unconfirmed("invalid_log_path", "a logs/REL.md path is required"))?;
    reject_sensitive_path(Path::new(&path))?;
    Ok(path)
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Disposition {
    OwnCurrent,
    Kept,
    OwnExpired,
    Invalid,
}

impl Disposition {
    fn finding(self) -> Option<(State, &'static str)> {
        match self {
            Disposition::OwnCurrent => Some((State::ReleasePending, "release_pending")),
            Disposition::Invalid => Some((State::Unconfirmed, "invalid_claim")),
            Disposition::Kept | Disposition::OwnExpired => None,
        }
    }
}

#[derive(Serialize)]
struct ClaimEvidence {
    path: String,
    blob: String,
    expires_at: Option<u64>,
    disposition: Disposition,
}

#[derive(Serialize)]
struct LogEvidence {
    path: String,
    blob: String,
}

#[derive(Serialize)]
struct FinishReceipt {
    schema_version: u32,
    product: &'static str,
    command: &'static str,
    state: State,
    checked_at: u64,
    agent_id: String,
    branch: String,
    requested_paths: Vec<String>,
    kiwi_head: Option<String>,
    kiwi_upstream: Option<Upstream>,
    project_remote: Option<String>,
    project_head: Option<String>,
    project_upstream: Option<Upstream>,
    upstream_source: &'static str,
    log: Option<LogEvidence>,
    claims: Vec<ClaimEvidence>,
    findings: Vec<Finding>,
}

impl FinishReceipt {
    fn new(options: &FinishOptions) -> Self {
        Self {
            schema_version: 1,
            product: "Kiwi",
            command: "finish",
            state: State::Finished,
            checked_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |t| t.as_secs()),
            agent_id: options.agent.clone(),
            branch: options.branch.clone(),
            requested_paths: options.paths.clone(),
            kiwi_head: None,
            kiwi_upstream: None,
            project_remote: None,
            project_head: None,
            project_upstream: None,
            upstream_source: "local_remote_tracking_ref",
            log: None,
            claims: Vec::new(),
            findings: Vec::new(),
        }
    }

    fn record(&mut self, problem: Finding) {
        self.state = self.state.max(problem.state);
        self.findings.push(problem);
    }

    fn write(&self, file: &mut File) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|_| unconfirmed("receipt_encode_failed", "receipt"))?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| unconfirmed("receipt_write_failed", "receipt"))
    }
}

fn judge(options: &FinishOptions, receipt: &mut FinishReceipt) -> Result<()> {
    let kiwi = Repository::open(&options.kiwi)?;
    receipt.kiwi_head = Some(kiwi.head.clone());
    let kiwi_remote = kiwi.remote()?;
    if !board::is_kiwi(&kiwi_remote) {
        return Err(unconfirmed("unverified_kiwi_repository", kiwi_remote));
    }
    receipt.kiwi_upstream = kiwi.upstream()?;
    if receipt.kiwi_upstream.as_ref().map(|u| &u.commit) != Some(&kiwi.head) {
        return Err(unconfirmed(
            "kiwi_not_synced",
            "Kiwi HEAD differs from its local upstream ref",
        ));
    }
    let claims = board::snapshot(&kiwi)?;

    let project = Repository::open(&options.project)?;
    receipt.project_head = Some(project.head.clone());
    let branch = project.branch()?;
    if branch != options.branch {
        return Err(unconfirmed("branch_mismatch", branch));
    }
    let project_remote = project.remote()?;
    receipt.project_remote = Some(project_remote.clone());

    let own_receipt = fs::canonicalize(&options.receipt)
        .map_err(|_| unconfirmed("receipt_unreadable", "receipt"))?;
    for path in project.changed(&options.paths)? {
        if project.root.join(&path) != own_receipt {
            receipt.record(finding(State::Incomplete, "uncommitted_changes", path));
        }
    }

    receipt.project_upstream = project.upstream()?;
    match &receipt.project_upstream {
        None => receipt.record(finding(State::Incomplete, "no_upstream", branch)),
        Some(upstream) if upstream.commit != project.head => receipt.record(finding(
            State::Incomplete,
            "head_not_on_upstream",
            upstream.name.clone(),
        )),
        Some(_) => {}
    }

    if let Some(log) = &options.log {
        match log_evidence(&kiwi, log, &options.branch) {
            Ok(evidence) => receipt.log = Some(evidence),
            Err(problem) => receipt.record(problem),
        }
    }

    for claim in claims {
        let Some(disposition) = classify(&claim, options, &project_remote, receipt.checked_at)
        else {
            continue;
        };
        if let Some((state, code)) = disposition.finding() {
            receipt.record(finding(state, code, claim.path.clone()));
        }
        receipt.claims.push(ClaimEvidence {
            expires_at: claim.expires_at(),
            path: claim.path,
            blob: claim.blob,
            disposition,
        });
    }
    Ok(())
}

fn log_evidence(kiwi: &Repository, path: &str, branch: &str) -> Result<LogEvidence> {
    let entry = kiwi.entries(path)?.into_iter().find(|e| e.path == path);
    if !kiwi.changed(&[path])?.is_empty() {
        return Err(unconfirmed("source_not_committed", path));
    }
    let entry = entry.ok_or_else(|| finding(State::Incomplete, "log_missing_branch", path))?;
    if !entry.regular() {
        return Err(unconfirmed("source_not_regular", path));
    }
    let size: u64 = kiwi
        .text(&["cat-file", "-s", &entry.blob])?
        .trim()
        .parse()
        .map_err(|_| unconfirmed("invalid_blob_size", path))?;
    if size > MAX_LOG {
        return Err(unconfirmed("source_too_large", path));
    }
    let body = kiwi.git(&["cat-file", "blob", &entry.blob])?;
    if !body
        .windows(branch.len())
        .any(|window| window == branch.as_bytes())
    {
        return Err(finding(State::Incomplete, "log_missing_branch", path));
    }
    Ok(LogEvidence {
        path: entry.path,
        blob: entry.blob,
    })
}

fn classify(
    claim: &ClaimFile,
    options: &FinishOptions,
    project_remote: &str,
    now: u64,
) -> Option<Disposition> {
    let Some(repository) = claim.field("repo").and_then(|r| canonical_remote(r).ok()) else {
        return Some(Disposition::Invalid);
    };
    let agent = claim.field("agent_id");
    let mine = agent.is_some_and(|a| a.eq_ignore_ascii_case(&options.agent));
    let expired = claim.expires_at().is_some_and(|at| at <= now);
    let elsewhere = (mine && expired).then_some(Disposition::OwnExpired);
    if !board::same_repository(&repository, project_remote) {
        return elsewhere;
    }
    match claim.field("branch") {
        None => Some(Disposition::Invalid),
        Some(branch) if branch != options.branch => elsewhere,
        Some(_) if !agent.is_some_and(uuid) => Some(Disposition::Invalid),
        Some(_) if options.keep_claim => mine.then_some(Disposition::Kept),
        Some(_) => mine.then_some(Disposition::OwnCurrent),
    }
}

fn print(value: &impl Serialize) {
    println!("{}", serde_json::to_string_pretty(value).unwrap());
}

fn rejected(problem: Finding) -> i32 {
    print(&problem);
    problem.state.exit_code()
}

pub fn usage() -> i32 {
    rejected(unconfirmed("usage", USAGE))
}

pub fn main(args: impl Iterator<Item = String>) -> i32 {
    let options = match FinishOptions::parse(args) {
        Ok(options) => options,
        Err(problem) => return rejected(problem),
    };
    let mut receipt = FinishReceipt::new(&options);
    let written = receipt_file(&options.receipt).and_then(|mut file| {
        if let Err(stop) = judge(&options, &mut receipt) {
            receipt.record(stop);
        }
        receipt.write(&mut file)
    });
    if let Err(problem) = written {
        receipt.record(problem);
    }
    print(&receipt);
    receipt.state.exit_code()
}
