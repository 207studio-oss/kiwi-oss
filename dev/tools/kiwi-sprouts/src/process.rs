use crate::compat::{dumps, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub fn which(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    let candidates = if path.components().count() > 1 || path.is_absolute() {
        vec![path.to_path_buf()]
    } else {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|dir| dir.join(name))
            .collect()
    };
    for candidate in candidates {
        #[cfg(windows)]
        {
            let extensions =
                std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
            if candidate.extension().is_some() && candidate.is_file() {
                return Some(candidate);
            }
            for extension in extensions.split(';') {
                let file = PathBuf::from(format!(
                    "{}{}",
                    candidate.display(),
                    extension.to_lowercase()
                ));
                if file.is_file() {
                    return Some(file);
                }
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if candidate.is_file()
                && candidate
                    .metadata()
                    .ok()
                    .is_some_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
            {
                return Some(candidate);
            }
        }
    }
    None
}

pub fn command(exe: &Path) -> Command {
    let mut command = Command::new(exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command.stdin(Stdio::null());
    command
}

pub fn child_pipe<T>(
    value: Option<T>,
    child: &mut std::process::Child,
    operation: &str,
) -> Result<T> {
    value.ok_or_else(|| {
        let _ = child.kill();
        let _ = child.wait();
        format!("'NoneType' object has no attribute '{operation}'")
    })
}

pub fn spawn_error(error: &std::io::Error, exe: &Path) -> String {
    #[cfg(windows)]
    if let Some(code) = error.raw_os_error() {
        return format!(
            "[WinError {code}] {}",
            error
                .to_string()
                .split(" (os error")
                .next()
                .unwrap()
                .trim_end_matches('.')
        );
    }
    crate::compat::file_error(error, exe)
}

pub struct Output {
    pub code: i32,
    pub out: Option<String>,
    pub err: Option<String>,
}

static THREAD_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn reserve_threads(count: u64) -> u64 {
    THREAD_ID.fetch_add(count, std::sync::atomic::Ordering::Relaxed) + 1
}

fn decode(bytes: Vec<u8>, thread: u64) -> Result<Option<String>> {
    if let Some(text) =
        crate::compat::text_encoding().decode_without_bom_handling_and_without_replacement(&bytes)
    {
        return Ok(Some(text.replace("\r\n", "\n").replace('\r', "\n")));
    }
    let exe = python()?;
    let encoding = if crate::compat::text_encoding() == encoding_rs::EUC_KR {
        "cp949"
    } else if crate::compat::text_encoding() == encoding_rs::UTF_8 {
        "utf-8"
    } else {
        crate::compat::text_encoding().name()
    };
    let script="import io,json,subprocess,sys,threading\nr=json.loads(sys.stdin.read())\nf=io.TextIOWrapper(io.BytesIO(bytes(r['bytes'])),encoding=r['encoding'])\nif sys.platform=='win32':\n t=threading.Thread(target=subprocess.Popen._readerthread,args=(None,f,[]),name='Thread-%s (_readerthread)'%r['thread']);t.start();t.join()\nelse:\n try:f.read()\n except Exception as e:print(str(e))";
    let mut child = command(&exe)
        .args(["-c", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let request = serde_json::json!({"bytes":bytes,"encoding":encoding,"thread":thread});
    let input = child.stdin.take();
    child_pipe(input, &mut child, "write")?
        .write_all(dumps(&request, None).as_bytes())
        .map_err(|error| error.to_string())?;
    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    if cfg!(windows) {
        Ok(None)
    } else {
        Err(String::from_utf8_lossy(&output.stdout).trim().into())
    }
}

pub fn run(exe: &Path, args: &[String], timeout: Option<u64>) -> Result<Output> {
    let mut child = command(exe)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| spawn_error(&error, exe))?;
    let thread = if cfg!(windows) { reserve_threads(2) } else { 0 };
    let stdout = child.stdout.take();
    let mut stdout = child_pipe(stdout, &mut child, "read")?;
    let stderr = child.stderr.take();
    let mut stderr = child_pipe(stderr, &mut child, "read")?;
    let out = std::thread::spawn(move || {
        let mut bytes = vec![];
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let err = std::thread::spawn(move || {
        let mut bytes = vec![];
        let _ = stderr.read_to_end(&mut bytes);
        bytes
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait().map_err(|error| error.to_string())? {
            Some(status) => break status,
            None if timeout
                .is_some_and(|seconds| start.elapsed() >= Duration::from_secs(seconds)) =>
            {
                let _ = child.kill();
                let _ = child.wait();
                let mut argv = vec![exe.to_string_lossy().to_string()];
                argv.extend_from_slice(args);
                return Err(format!(
                    "Command '{}' timed out after {} seconds",
                    python_argv(&argv),
                    timeout.unwrap()
                ));
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;
    #[cfg(unix)]
    let code = status
        .code()
        .unwrap_or_else(|| -status.signal().unwrap_or(1));
    #[cfg(windows)]
    let code = status.code().unwrap_or(1);
    Ok(Output {
        code,
        out: decode(out.join().unwrap_or_default(), thread)?,
        err: decode(err.join().unwrap_or_default(), thread + 1)?,
    })
}

pub fn python() -> Result<PathBuf> {
    let name = if cfg!(windows) { "python" } else { "python3" };
    which(name).ok_or_else(|| format!("language server not found: {name} (run bootstrap.sh)"))
}

pub fn python_cli(args: &[String]) -> Result<i32> {
    let status = command(&python()?)
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("kiwi_sprouts.py"))
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|error| error.to_string())?;
    Ok(status.code().unwrap_or(1))
}

pub fn report_mcp_error(root: &Path, line: &str) -> Result<i32> {
    let script="import json,subprocess,sys\nr=json.loads(sys.stdin.buffer.read())\np=subprocess.run([sys.executable,r['script'],'--root',r['root'],'mcp'],input=r['line'].encode(sys.stdin.encoding,sys.stdin.errors))\nraise SystemExit(p.returncode)";
    let mut child = command(&python()?)
        .args(["-c", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| error.to_string())?;
    let request = serde_json::json!({"script":Path::new(env!("CARGO_MANIFEST_DIR")).join("kiwi_sprouts.py"),"root":root,"line":line});
    let input = child.stdin.take();
    child_pipe(input, &mut child, "write")?
        .write_all(dumps(&request, None).as_bytes())
        .map_err(|error| error.to_string())?;
    child
        .wait()
        .map(|status| status.code().unwrap_or(1))
        .map_err(|error| error.to_string())
}

fn python_argv(args: &[String]) -> String {
    format!(
        "[{}]",
        args.iter()
            .map(|arg| format!("'{}'", arg.replace('\\', "\\\\").replace('\'', "\\'")))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

pub struct JsonWorker {
    child: Option<std::process::Child>,
    output: Option<std::io::BufReader<std::process::ChildStdout>>,
    script: &'static str,
    pub fatal_code: Option<i32>,
}

impl JsonWorker {
    pub fn new(script: &'static str) -> Self {
        Self {
            child: None,
            output: None,
            script,
            fatal_code: None,
        }
    }

    pub fn call(&mut self, request: serde_json::Value) -> Result<serde_json::Value> {
        use std::io::BufRead;
        if self.child.is_none() {
            let mut child = command(&python()?)
                .args(["-u", "-c", self.script])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .map_err(|error| error.to_string())?;
            let output = child.stdout.take();
            self.output = Some(std::io::BufReader::new(child_pipe(
                output, &mut child, "readline",
            )?));
            self.child = Some(child);
        }
        let child = self
            .child
            .as_mut()
            .ok_or("'NoneType' object has no attribute 'stdin'")?;
        let stdin = child
            .stdin
            .as_mut()
            .ok_or("'NoneType' object has no attribute 'write'")?;
        writeln!(stdin, "{}", dumps(&request, None)).map_err(|error| error.to_string())?;
        stdin.flush().map_err(|error| error.to_string())?;
        let mut line = String::new();
        self.output
            .as_mut()
            .ok_or("'NoneType' object has no attribute 'readline'")?
            .read_line(&mut line)
            .map_err(|error| error.to_string())?;
        let reply = crate::protocol::parse_json(&line)?;
        if reply["fatal"] == true {
            self.fatal_code = Some(
                self.child
                    .as_mut()
                    .ok_or("'NoneType' object has no attribute 'wait'")?
                    .wait()
                    .map_err(|error| error.to_string())?
                    .code()
                    .unwrap_or(1),
            );
            return Err(String::new());
        }
        if let Some(error) = reply.get("error") {
            Err(crate::compat::pystr(error))
        } else {
            Ok(reply["result"].clone())
        }
    }
}

impl Drop for JsonWorker {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            child.stdin.take();
            let _ = child.wait();
        }
    }
}
