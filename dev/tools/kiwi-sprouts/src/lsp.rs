use crate::compat::{dumps, read_text, Result};
use crate::process;
use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, CONTROLS};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

const QUOTED: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'!')
    .add(b'"')
    .add(b'#')
    .add(b'$')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

pub fn uri(path: &Path) -> Result<String> {
    let resolved = crate::compat::resolved(path)?;
    let text = resolved.to_string_lossy();
    if cfg!(windows) {
        let text = text.replace('\\', "/");
        return Ok(if text.starts_with("//") {
            format!("file:{}", utf8_percent_encode(&text, QUOTED))
        } else {
            format!(
                "file:///{}{}",
                &text[..2],
                utf8_percent_encode(&text[2..], QUOTED)
            )
        });
    }
    Ok(format!("file://{}", utf8_percent_encode(&text, QUOTED)))
}
pub fn unuri(text: &str) -> String {
    let decoded = percent_decode_str(text.strip_prefix("file://").unwrap_or(text))
        .decode_utf8_lossy()
        .into_owned();
    if cfg!(windows) && text.starts_with("file://") {
        if decoded.starts_with('/') && decoded.as_bytes().get(2) == Some(&b':') {
            return decoded[1..].replace('/', "\\");
        }
        if !decoded.starts_with('/') && decoded.as_bytes().get(1) != Some(&b':') {
            return format!("\\\\{}", decoded.replace('/', "\\"));
        }
        return decoded.replace('/', "\\");
    }
    decoded
}

pub fn language(path: &Path) -> Option<(&'static str, &'static [&'static str], &'static str)> {
    match path.extension()?.to_str()? {
        "py" => Some(("pyright-langserver", &["--stdio"], "python")),
        "ts" => Some(("typescript-language-server", &["--stdio"], "typescript")),
        "tsx" => Some((
            "typescript-language-server",
            &["--stdio"],
            "typescriptreact",
        )),
        "js" => Some(("typescript-language-server", &["--stdio"], "javascript")),
        "rs" => Some(("rust-analyzer", &[], "rust")),
        "swift" => Some(("sourcekit-lsp", &[], "swift")),
        _ => None,
    }
}

#[derive(Default)]
struct Replies {
    values: HashMap<String, Value>,
    errors: VecDeque<String>,
    eof: bool,
}

pub struct Lsp {
    pub key: String,
    child: Child,
    input: Arc<Mutex<ChildStdin>>,
    replies: Arc<(Mutex<Replies>, Condvar)>,
    threads: Vec<std::thread::JoinHandle<()>>,
    id: u64,
    opened: HashSet<String>,
}

fn send(input: &Mutex<ChildStdin>, message: &Value) -> std::io::Result<()> {
    let bytes = dumps(message, None).into_bytes();
    let mut input = input.lock().unwrap();
    write!(input, "Content-Length: {}\r\n\r\n", bytes.len())?;
    input.write_all(&bytes)?;
    input.flush()
}

fn append_error(replies: &mut Replies, text: String) {
    if replies.errors.len() == 20 {
        replies.errors.pop_front();
    }
    replies.errors.push_back(text);
}

fn pipe<T>(value: Option<T>, child: &mut Child, message: &str) -> Result<T> {
    value.ok_or_else(|| {
        let _ = child.kill();
        let _ = child.wait();
        message.to_string()
    })
}

impl Lsp {
    pub fn new(key: &str, args: &[&str], root: &Path, options: Option<Value>) -> Result<Self> {
        let exe = process::which(key)
            .ok_or_else(|| format!("language server not found: {key} (run bootstrap.sh)"))?;
        let mut child = process::command(&exe)
            .args(args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| process::spawn_error(&error, &exe))?;
        let input = child.stdin.take();
        let input = Arc::new(Mutex::new(pipe(
            input,
            &mut child,
            "'NoneType' object has no attribute 'write'",
        )?));
        let output = child.stdout.take();
        let mut output = BufReader::new(pipe(
            output,
            &mut child,
            "'NoneType' object has no attribute 'readline'",
        )?);
        let error = child.stderr.take();
        let error = BufReader::new(pipe(
            error,
            &mut child,
            "'NoneType' object is not iterable",
        )?);
        let replies = Arc::new((Mutex::new(Replies::default()), Condvar::new()));
        process::reserve_threads(2);
        let reader_replies = replies.clone();
        let reader_input = input.clone();
        let reader = std::thread::spawn(move || {
            let read = || -> Result<()> {
                loop {
                    let mut size = 0;
                    loop {
                        let mut line = String::new();
                        if output
                            .read_line(&mut line)
                            .map_err(|error| error.to_string())?
                            == 0
                        {
                            return Ok(());
                        }
                        if line == "\n" || line == "\r\n" {
                            break;
                        }
                        if line.to_lowercase().starts_with("content-length:") {
                            size = line
                                .split(':')
                                .nth(1)
                                .unwrap_or("")
                                .trim()
                                .parse()
                                .map_err(|error: std::num::ParseIntError| error.to_string())?;
                        }
                    }
                    let mut bytes = vec![0; size];
                    output
                        .read_exact(&mut bytes)
                        .map_err(|error| error.to_string())?;
                    let text = std::str::from_utf8(&bytes).map_err(|error| error.to_string())?;
                    let message = crate::protocol::parse_json(text)?;
                    if let Some(id) = message.get("id") {
                        if let Some(method) = message.get("method") {
                            let result = if method == "workspace/configuration" {
                                Value::Array(vec![
                                    Value::Null;
                                    message["params"]["items"]
                                        .as_array()
                                        .map_or(0, Vec::len)
                                ])
                            } else {
                                Value::Null
                            };
                            let mut reply = json!({"jsonrpc":"2.0","id":null,"result":null});
                            reply["id"] = id.clone();
                            reply["result"] = result;
                            send(&reader_input, &reply).map_err(|error| error.to_string())?;
                        } else {
                            reader_replies
                                .0
                                .lock()
                                .unwrap()
                                .values
                                .insert(id.to_string(), message.clone());
                            reader_replies.1.notify_all();
                        }
                    }
                }
            };
            let result = {
                let mut read = read;
                read()
            };
            let mut state = reader_replies.0.lock().unwrap();
            if let Err(error) = result {
                append_error(&mut state, format!("read error: {error}"));
            }
            state.eof = true;
            reader_replies.1.notify_all();
        });
        let error_replies = replies.clone();
        let errors = std::thread::spawn(move || {
            for bytes in error.split(b'\n').flatten() {
                append_error(
                    &mut error_replies.0.lock().unwrap(),
                    String::from_utf8_lossy(&bytes).trim().into(),
                );
            }
        });
        let mut server = Self {
            key: key.into(),
            child,
            input,
            replies,
            threads: vec![reader, errors],
            id: 0,
            opened: HashSet::new(),
        };
        let root_uri = uri(root)?;
        let mut params = json!({"processId":std::process::id(),"rootUri":root_uri,"workspaceFolders":[{"uri":root_uri,"name":"root"}],"capabilities":{"workspace":{"configuration":true},"textDocument":{"documentSymbol":{"hierarchicalDocumentSymbolSupport":true}}}});
        if let Some(options) = options {
            params["initializationOptions"] = options;
        }
        server.req("initialize", params, 60)?;
        server.note("initialized", json!({}))?;
        Ok(server)
    }

    pub fn note(&self, method: &str, params: Value) -> Result<()> {
        self.write(&json!({"jsonrpc":"2.0","method":method,"params":params}))
    }

    fn write(&self, value: &Value) -> Result<()> {
        send(&self.input, value).map_err(|_| format!("{} is not running", self.key))
    }

    pub fn req(&mut self, method: &str, params: Value, timeout: u64) -> Result<Value> {
        self.id += 1;
        self.write(&json!({"jsonrpc":"2.0","id":self.id,"method":method,"params":params}))?;
        let key = self.id.to_string();
        let (mut state, timed) = self
            .replies
            .1
            .wait_timeout_while(
                self.replies.0.lock().unwrap(),
                Duration::from_secs(timeout),
                |state| !state.values.contains_key(&key) && !state.eof,
            )
            .map_err(|_| "cannot wait on un-acquired lock".to_string())?;
        if let Some(message) = state.values.remove(&key) {
            if let Some(error) = message.get("error") {
                return Err(crate::compat::pystr(&error["message"]));
            }
            Ok(message.get("result").cloned().unwrap_or(Value::Null))
        } else if timed.timed_out() {
            Err(method.into())
        } else {
            let tail = if state.errors.is_empty() {
                "no stderr".into()
            } else {
                state.errors.iter().cloned().collect::<Vec<_>>().join("; ")
            };
            Err(format!(
                "{} exited before answering {method}: {tail}",
                self.key
            ))
        }
    }

    pub fn open(&mut self, path: &Path) -> Result<()> {
        let path = crate::compat::resolved(path)?;
        let key = path.to_string_lossy().to_string();
        if !self.opened.contains(&key) {
            let (_, _, language_id) = language(&path).ok_or_else(|| {
                format!(
                    "'.{}'",
                    path.extension().unwrap_or_default().to_string_lossy()
                )
            })?;
            self.note("textDocument/didOpen", json!({"textDocument":{"uri":uri(&path)?,"languageId":language_id,"version":1,"text":read_text(&path,false)?}}))?;
            self.opened.insert(key);
        }
        Ok(())
    }
}

impl Drop for Lsp {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            if self.req("shutdown", Value::Null, 3).is_ok() {
                let _ = self.note("exit", Value::Null);
            }
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while self.child.try_wait().ok().flatten().is_none()
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            if self.child.try_wait().ok().flatten().is_none() {
                let _ = self.child.kill();
            }
        }
        let _ = self.child.wait();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}
