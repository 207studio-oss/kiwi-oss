use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, Read, Write};

fn data() -> Value {
    std::env::var_os("SPROUTS_FIXTURE_DATA")
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(json!({}))
}

fn frame(message: Value) {
    let bytes = serde_json::to_vec(&message).unwrap();
    print!("Content-Length: {}\r\n\r\n", bytes.len());
    std::io::stdout().write_all(&bytes).unwrap();
    std::io::stdout().flush().unwrap();
}

fn frame_raw(message: &str) {
    print!("Content-Length: {}\r\n\r\n{message}", message.len());
    std::io::stdout().flush().unwrap();
}

fn lsp() {
    let mut input = std::io::stdin().lock();
    let mut documents = HashMap::new();
    let mut attempts: HashMap<String, usize> = HashMap::new();
    loop {
        let mut size = 0;
        loop {
            let mut line = String::new();
            if input.read_line(&mut line).unwrap() == 0 {
                return;
            }
            if line.trim().is_empty() {
                break;
            }
            if let Some(length) = line.strip_prefix("Content-Length:") {
                size = length.trim().parse().unwrap();
            }
        }
        let mut bytes = vec![0; size];
        input.read_exact(&mut bytes).unwrap();
        let Ok(text) = std::str::from_utf8(&bytes) else {
            return;
        };
        let Ok(request) = kiwi_sprouts::protocol::parse_json(text) else {
            return;
        };
        if request.get("method").is_none() {
            continue;
        }
        let method = request["method"].as_str().unwrap_or("");
        if method == "exit" {
            return;
        }
        if method == "textDocument/didOpen" {
            documents.insert(
                request["params"]["textDocument"]["uri"]
                    .as_str()
                    .unwrap()
                    .to_string(),
                request["params"]["textDocument"]["text"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            );
        }
        if request.get("id").is_none() {
            continue;
        }
        let configuration = data();
        if configuration["lsp"]["exit"] == method {
            return;
        }
        if let Some(result) = configuration["lsp"]["raw"][method].as_str() {
            let message = format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{result}}}",
                request["id"]
            );
            frame_raw(&message);
            continue;
        }
        let result = match method {
            "initialize" => {
                if let Some(requests) = configuration["lsp"]["requests"].as_array() {
                    for request in requests.iter().filter_map(Value::as_str) {
                        frame_raw(request);
                    }
                }
                frame(
                    json!({"jsonrpc":"2.0","id":"config","method":"workspace/configuration","params":{"items":[{},{}]}}),
                );
                json!({"capabilities":{}})
            }
            "textDocument/documentSymbol" => {
                let uri = request["params"]["textDocument"]["uri"].as_str().unwrap();
                if let Some(symbols) = configuration["lsp"].get("symbols") {
                    symbols.clone()
                } else {
                    let text = documents.get(uri).cloned().unwrap_or_default();
                    let mut symbols = vec![];
                    for (line, text) in text.lines().enumerate() {
                        let trimmed = text.trim_start();
                        let prefix = [
                            "def ",
                            "class ",
                            "fn ",
                            "pub fn ",
                            "function ",
                            "export function ",
                            "func ",
                            "struct ",
                        ]
                        .into_iter()
                        .find(|prefix| trimmed.starts_with(prefix));
                        if let Some(prefix) = prefix {
                            let rest = &trimmed[prefix.len()..];
                            let name: String = rest
                                .chars()
                                .take_while(|c| c.is_alphanumeric() || *c == '_')
                                .collect();
                            let col = text.find(&name).unwrap();
                            let lines: Vec<_> = documents[uri].lines().collect();
                            let mut end = line;
                            if uri.ends_with(".py") {
                                while end + 1 < lines.len()
                                    && (lines[end + 1].starts_with(' ')
                                        || lines[end + 1].is_empty())
                                {
                                    end += 1;
                                }
                            }
                            let kind = if prefix == "class " { 5 } else { 12 };
                            symbols.push(json!({"name":name,"kind":kind,"range":{"start":{"line":line,"character":text.len()-trimmed.len()},"end":{"line":end,"character":lines[end].len()}},"selectionRange":{"start":{"line":line,"character":col},"end":{"line":line,"character":col+rest.chars().take_while(|c|c.is_alphanumeric()||*c=='_').count()}}}));
                        }
                    }
                    Value::Array(symbols)
                }
            }
            "textDocument/definition"
            | "textDocument/references"
            | "textDocument/documentHighlight" => {
                let entry = attempts.entry(method.into()).or_default();
                *entry += 1;
                if configuration["lsp"]["retry"]
                    .as_u64()
                    .is_some_and(|count| (*entry as u64) <= count)
                {
                    frame(
                        json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32801,"message":"content modified"}}),
                    );
                    continue;
                }
                let key = match method {
                    "textDocument/definition" => "definition",
                    "textDocument/references" => "references",
                    _ => "highlights",
                };
                if configuration["suite"] == true && configuration["lsp"].get(key).is_none() {
                    let uri = request["params"]["textDocument"]["uri"].as_str().unwrap();
                    let (line, col) = if uri.ends_with(".py") {
                        (1, 8)
                    } else if uri.ends_with(".ts") {
                        (0, 16)
                    } else if uri.ends_with(".swift") {
                        (0, 5)
                    } else {
                        (0, 3)
                    };
                    let rg = json!({"start":{"line":line,"character":col},"end":{"line":line,"character":col+3}});
                    let result = if key == "definition" {
                        json!([{"uri":uri,"range":rg}])
                    } else if key == "references" {
                        json!([{"uri":uri,"range":{"start":{"line":3,"character":6},"end":{"line":3,"character":9}}},{"uri":uri,"range":{"start":{"line":4,"character":6},"end":{"line":4,"character":9}}}])
                    } else {
                        json!([])
                    };
                    frame(json!({"jsonrpc":"2.0","id":request["id"],"result":result}));
                    continue;
                }
                let mut result = configuration["lsp"].get(key).cloned().unwrap_or(json!([]));
                if let Some(list) = result.as_array_mut() {
                    for location in list {
                        for key in ["uri", "targetUri"] {
                            if location[key] == "$DOCUMENT" {
                                location[key] = request["params"]["textDocument"]["uri"].clone();
                            } else if let Some(path) = location[key]
                                .as_str()
                                .and_then(|s| s.strip_prefix("$FILE:"))
                            {
                                location[key] = json!(format!("file://{}", path));
                            }
                        }
                    }
                }
                result
            }
            _ => Value::Null,
        };
        frame(json!({"jsonrpc":"2.0","id":request["id"],"result":result}));
    }
}

fn main() {
    let exe = std::env::current_exe().unwrap();
    let name = exe.file_stem().unwrap().to_string_lossy();
    if [
        "pyright-langserver",
        "typescript-language-server",
        "rust-analyzer",
        "sourcekit-lsp",
    ]
    .contains(&name.as_ref())
    {
        lsp();
        return;
    }
    let config = data();
    let args: Vec<_> = std::env::args().skip(1).collect();
    if let Some(path) = config["record"].as_str() {
        use std::fs::OpenOptions;
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
            .unwrap();
        writeln!(file, "{}", json!({"name":name,"args":args})).unwrap();
    }
    let section = if name.starts_with("kiwi-graph") {
        "graph"
    } else if name.starts_with("kiwi-search")
        || name.starts_with("brain-search")
        || name.starts_with("fake-docs-search")
    {
        "docs"
    } else {
        "kiwi"
    };
    let result = &config[section];
    if let Some(text) = result["out"].as_str() {
        print!("{text}");
    } else if let Some(value) = result.get("json") {
        println!("{value}");
    } else {
        println!("{}", json!({"args":args,"unicode":"키위 🌱"}));
    }
    if let Some(text) = result["err"].as_str() {
        eprint!("{text}");
    }
    std::process::exit(result["exit"].as_i64().unwrap_or(0) as i32);
}
