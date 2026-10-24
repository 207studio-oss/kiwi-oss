use std::io::{self, Read};
use std::path::Path;
use std::process::{Command, Output, Stdio};

const MAX_OUTPUT: u64 = 16 * 1024 * 1024;
const REPOSITORY_SELECTORS: [&str; 15] = [
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

pub enum Failure {
    Unavailable,
    TooLarge,
}

pub fn output(dir: &Path, args: &[&str]) -> Result<Output, Failure> {
    let mut command = Command::new("git");
    command
        .args([
            "--no-replace-objects",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-C",
        ])
        .arg(dir)
        .args(args)
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C");
    for name in REPOSITORY_SELECTORS {
        command.env_remove(name);
    }
    bounded(command, MAX_OUTPUT)
}

fn bounded(mut command: Command, limit: u64) -> Result<Output, Failure> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| Failure::Unavailable)?;
    let mut errors = child.stderr.take().ok_or(Failure::Unavailable)?;
    let stderr = std::thread::spawn(move || {
        let mut kept = Vec::new();
        let _ = Read::by_ref(&mut errors).take(limit).read_to_end(&mut kept);
        let _ = io::copy(&mut errors, &mut io::sink());
        kept
    });
    let mut stdout = Vec::new();
    let read = child
        .stdout
        .take()
        .ok_or(Failure::Unavailable)?
        .take(limit + 1)
        .read_to_end(&mut stdout);
    if read.is_err() || stdout.len() as u64 > limit {
        let _ = child.kill();
        let _ = child.wait();
        return Err(match read {
            Err(_) => Failure::Unavailable,
            Ok(_) => Failure::TooLarge,
        });
    }
    let status = child.wait().map_err(|_| Failure::Unavailable)?;
    let stderr = stderr.join().map_err(|_| Failure::Unavailable)?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn output_past_the_bound_is_rejected_without_waiting_for_the_process() {
        let mut command = Command::new("sh");
        command.args(["-c", "head -c 2048 /dev/zero; exec sleep 20"]);
        let started = Instant::now();
        assert!(matches!(bounded(command, 1024), Err(Failure::TooLarge)));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn every_repository_variable_git_reports_is_cleared() {
        let listed = Command::new("git")
            .args(["rev-parse", "--local-env-vars"])
            .output()
            .unwrap();
        assert!(listed.status.success());
        for name in String::from_utf8(listed.stdout).unwrap().split_whitespace() {
            assert!(REPOSITORY_SELECTORS.contains(&name), "{name} is kept");
        }
    }
}
