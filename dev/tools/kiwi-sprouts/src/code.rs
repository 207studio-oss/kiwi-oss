use crate::compat::{denied, read_text, relative_required, resolved, splitlines, Result};
use crate::lsp::{language, unuri, uri, Lsp};
use crate::Sprouts;
use num_bigint::BigInt;
use num_traits::{FromPrimitive, ToPrimitive};
use serde_json::{json, Value};
use std::collections::hash_map::Entry;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

impl Sprouts {
    pub fn abs(&self, path: &str) -> Result<PathBuf> {
        let path = resolved(&crate::compat::joined(&self.root, path))?;
        let relative = path
            .strip_prefix(&self.root)
            .map_err(|_| "path outside root".to_string())?;
        if denied(relative) {
            return Err("path denied".into());
        }
        Ok(path)
    }

    fn lsp(&mut self, path: &Path) -> Result<&mut Lsp> {
        let (key, args, _) = language(path).ok_or_else(|| {
            format!(
                "unsupported file type: {}",
                path.extension()
                    .map(|ext| format!(".{}", ext.to_string_lossy()))
                    .unwrap_or_default()
            )
        })?;
        let server = match self.servers.entry(key.into()) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let ts = if key == "typescript-language-server" {
                    tsserver_path()
                } else {
                    None
                };
                let server = Lsp::new(
                    key,
                    args,
                    &self.root,
                    ts.map(|path| json!({"tsserver":{"path":path}})),
                )?;
                entry.insert(server)
            }
        };
        server.open(path)?;
        Ok(server)
    }

    fn symbols(&mut self, path: &Path) -> Result<Vec<Value>> {
        let values = self.request_symbols(path)?;
        self.flatten_symbols(&values, path)
    }

    fn request_symbols(&mut self, path: &Path) -> Result<Value> {
        self.lsp(path)?.req(
            "textDocument/documentSymbol",
            json!({"textDocument":{"uri":uri(path)?}}),
            60,
        )
    }

    fn flatten_symbols(&self, values: &Value, path: &Path) -> Result<Vec<Value>> {
        let mut flat = vec![];
        flatten(values, path, &self.root, "", &mut flat)?;
        Ok(flat)
    }

    pub fn files(&self, under: &str) -> Result<Vec<PathBuf>> {
        fn collect(base: &Path, root: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
            let Ok(entries) = std::fs::read_dir(base) else {
                return Ok(());
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let rel = relative_required(&path, root)?;
                if path.is_dir() && !entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
                    collect(&path, root, out)?;
                }
                if language(&path).is_some()
                    && !rel.components().any(|part| {
                        matches!(
                            part.as_os_str().to_str(),
                            Some(
                                ".git"
                                    | "node_modules"
                                    | "target"
                                    | ".build"
                                    | "__pycache__"
                                    | ".venv"
                                    | "dist"
                            )
                        )
                    })
                    && !denied(&rel)
                {
                    out.push(path);
                }
            }
            Ok(())
        }
        let base = self.abs(under)?;
        let mut files = vec![];
        if base.is_file() {
            files.push(base);
        } else {
            collect(&base, &self.root, &mut files)?;
        }
        if cfg!(windows) {
            files.sort_by_key(|path| path.to_string_lossy().to_lowercase());
        } else {
            files.sort();
        }
        files.truncate(400);
        Ok(files)
    }

    pub fn code_overview(&mut self, path: &str) -> Result<Value> {
        let path = self.abs(path)?;
        Ok(Value::Array(
            self.symbols(&path)?
                .into_iter()
                .map(without_range)
                .collect::<Result<Vec<_>>>()?,
        ))
    }

    pub fn code_find(&mut self, name: &str, path: &str) -> Result<Value> {
        let mut out = vec![];
        for file in self.files(path)? {
            let Ok(values) = self.request_symbols(&file) else {
                continue;
            };
            let symbols = self.flatten_symbols(&values, &file)?;
            for symbol in symbols {
                if symbol_name(&symbol)?
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&name.to_lowercase())
                {
                    out.push(without_range(symbol)?);
                }
            }
        }
        out.truncate(100);
        Ok(Value::Array(out))
    }

    fn locs(
        &mut self,
        method: &str,
        path: &str,
        line: &BigInt,
        col: &BigInt,
        extra: Option<Value>,
        retries: usize,
    ) -> Result<Vec<Value>> {
        let path = self.abs(path)?;
        let root = self.root.clone();
        let server = self.lsp(&path)?;
        for attempt in 0..retries {
            let mut params =
                json!({"textDocument":{"uri":uri(&path)?},"position":position(line,col)});
            if let Some(Value::Object(extra)) = &extra {
                if let Value::Object(params) = &mut params {
                    params.extend(extra.clone());
                }
            }
            let response = match server.req(method, params, 60) {
                Ok(value) => value,
                Err(error)
                    if attempt + 1 < retries
                        && ["content modified", "cancel", "not ready", "try again"]
                            .iter()
                            .any(|text| error.to_lowercase().contains(text)) =>
                {
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                }
                Err(error) => return Err(error),
            };
            let mut out = vec![];
            for location in as_list(response) {
                let file_uri = location
                    .get("uri")
                    .filter(|v| crate::compat::truthy(v))
                    .or_else(|| location.get("targetUri"));
                let range = location
                    .get("range")
                    .filter(|v| crate::compat::truthy(v))
                    .or_else(|| location.get("targetSelectionRange"));
                let (Some(file_uri), Some(range)) = (file_uri.and_then(Value::as_str), range)
                else {
                    continue;
                };
                let file: PathBuf = Path::new(&unuri(file_uri)).components().collect();
                let start = field(range, "start")?;
                let ln = add_one(field(start, "line")?)?;
                let text = if file.exists() {
                    line_text(&file, &ln)?
                } else {
                    String::new()
                };
                let mut location = json!({"path":crate::compat::relative(&file,&root).unwrap_or_else(||file.clone()).to_string_lossy(),"line":null,"col":null,"text":text});
                location["line"] = ln;
                location["col"] = add_one(field(start, "character")?)?;
                out.push(location);
            }
            if !out.is_empty() || attempt + 1 >= retries {
                return Ok(out);
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        Ok(vec![])
    }

    fn ident_at(&self, path: &str, line: &BigInt, col: &BigInt) -> Option<String> {
        let text = read_text(&self.abs(path).ok()?, false).ok()?;
        let lines = splitlines(&text);
        let line = line.to_usize()?;
        if line < 1 || line > lines.len() {
            return None;
        }
        let chars: Vec<_> = lines[line - 1].chars().collect();
        if chars.is_empty() {
            return None;
        }
        let mut index = (col - BigInt::from(1))
            .max(BigInt::from(0))
            .min(BigInt::from(chars.len() - 1))
            .to_usize()?;
        let identifier = |ch: char| ch.is_alphanumeric() || ch == '_';
        if !identifier(chars[index]) {
            if index + 1 < chars.len() && identifier(chars[index + 1]) {
                index += 1;
            } else if index > 0 && identifier(chars[index - 1]) {
                index -= 1;
            } else {
                return None;
            }
        }
        let mut start = index;
        let mut end = index;
        while start > 0 && identifier(chars[start - 1]) {
            start -= 1;
        }
        while end + 1 < chars.len() && identifier(chars[end + 1]) {
            end += 1;
        }
        Some(chars[start..=end].iter().collect())
    }

    fn fallback_definition(
        &mut self,
        path: &str,
        line: &BigInt,
        col: &BigInt,
    ) -> Result<Vec<Value>> {
        let Some(name) = self.ident_at(path, line, col) else {
            return Ok(vec![]);
        };
        let mut files = vec![self.abs(path)?];
        files.extend(self.files(".")?);
        let mut seen = HashSet::new();
        let mut out = vec![];
        for file in files {
            let file = resolved(&file)?;
            if !seen.insert(file.clone()) {
                continue;
            }
            let Ok(values) = self.request_symbols(&file) else {
                continue;
            };
            let symbols = self.flatten_symbols(&values, &file)?;
            for symbol in symbols {
                if symbol_name(&symbol)?.rsplit('/').next().unwrap_or("") != name {
                    continue;
                }
                let ln = field(&symbol, "line")?;
                let mut location = json!({"path":symbol["path"],"line":null,"col":null,"text":line_text(&file,ln)?});
                location["line"] = ln.clone();
                location["col"] = symbol["col"].clone();
                out.push(location);
                if out.len() >= 20 {
                    return Ok(out);
                }
            }
        }
        Ok(out)
    }

    fn highlight_refs(&mut self, path: &str, line: &BigInt, col: &BigInt) -> Result<Vec<Value>> {
        let file = self.abs(path)?;
        let response = match self.lsp(&file).and_then(|server| {
            server.req(
                "textDocument/documentHighlight",
                json!({"textDocument":{"uri":uri(&file)?},"position":position(line,col)}),
                60,
            )
        }) {
            Ok(value) => value,
            Err(_) => return Ok(vec![]),
        };
        let mut declarations = self
            .locs("textDocument/definition", path, line, col, None, 1)
            .unwrap_or_default();
        if declarations.is_empty() {
            declarations = self.fallback_definition(path, line, col)?;
        }
        let mut positions = positions(&declarations)?;
        if positions.is_empty() {
            positions.insert((
                Coordinate::Integer(line.clone()),
                Coordinate::Integer(col.clone()),
            ));
        }
        let mut out = vec![];
        for highlight in as_list(response) {
            let start = &highlight["range"]["start"];
            let ln = add_one(start.get("line").unwrap_or(&json!(0)))?;
            let ch = add_one(start.get("character").unwrap_or(&json!(0)))?;
            if position_key(&ln, &ch).is_some_and(|key| {
                !matches!(key, (Coordinate::Nan, _) | (_, Coordinate::Nan))
                    && positions.contains(&key)
            }) {
                continue;
            }
            let mut location = json!({"path":relative_required(&file,&self.root)?.to_string_lossy(),"line":null,"col":null,"text":line_text(&file,&ln)?});
            location["line"] = ln;
            location["col"] = ch;
            out.push(location);
        }
        Ok(out)
    }

    fn text_refs(&mut self, path: &str, line: &BigInt, col: &BigInt) -> Result<Vec<Value>> {
        let Some(name) = self.ident_at(path, line, col) else {
            return Ok(vec![]);
        };
        let mut declarations = positions(&self.fallback_definition(path, line, col)?)?;
        if declarations.is_empty() {
            declarations.insert((
                Coordinate::Integer(line.clone()),
                Coordinate::Integer(col.clone()),
            ));
        }
        let pattern = regex::Regex::new(&format!(r"\b{}\b", regex::escape(&name)))
            .map_err(|error| error.to_string())?;
        let mut out = vec![];
        for file in self.files(".")? {
            let Ok(text) = read_text(&file, false) else {
                continue;
            };
            for hit in pattern.find_iter(&text) {
                let prefix = &text[..hit.start()];
                let ln = prefix.matches('\n').count() as i64 + 1;
                let ch = prefix[prefix.rfind('\n').map_or(0, |position| position + 1)..]
                    .chars()
                    .count() as i64
                    + 1;
                if declarations.contains(&(
                    Coordinate::Integer(BigInt::from(ln)),
                    Coordinate::Integer(BigInt::from(ch)),
                )) {
                    continue;
                }
                out.push(json!({"path":relative_required(&file,&self.root)?.to_string_lossy(),"line":ln,"col":ch,"text":line_text(&file,&json!(ln))?}));
                if out.len() >= 200 {
                    return Ok(out);
                }
            }
        }
        Ok(out)
    }

    pub fn code_definition(&mut self, path: &str, line: &BigInt, col: &BigInt) -> Result<Value> {
        let file = self.abs(path)?;
        let out = self
            .lsp(&file)
            .map(|server| if server.key == "rust-analyzer" { 40 } else { 1 })
            .and_then(|retries| {
                self.locs("textDocument/definition", path, line, col, None, retries)
            })
            .unwrap_or_default();
        Ok(Value::Array(if out.is_empty() {
            self.fallback_definition(path, line, col)?
        } else {
            out
        }))
    }

    pub fn code_references(&mut self, path: &str, line: &BigInt, col: &BigInt) -> Result<Value> {
        let file = self.abs(path)?;
        let mut out = self
            .lsp(&file)
            .map(|server| if server.key == "rust-analyzer" { 40 } else { 1 })
            .and_then(|retries| {
                self.locs(
                    "textDocument/references",
                    path,
                    line,
                    col,
                    Some(json!({"context":{"includeDeclaration":false}})),
                    retries,
                )
            })
            .unwrap_or_default();
        if out.is_empty() {
            out = self.highlight_refs(path, line, col)?;
        }
        if out.is_empty() {
            out = self.text_refs(path, line, col)?;
        }
        Ok(Value::Array(out))
    }

    pub fn code_replace_body(&mut self, path: &str, name: &str, body: &str) -> Result<Value> {
        let file = self.abs(path)?;
        let hits: Vec<_> = self
            .symbols(&file)?
            .into_iter()
            .filter(|symbol| {
                symbol["name"]
                    .as_str()
                    .is_some_and(|value| value == name || value.ends_with(&format!("/{name}")))
            })
            .collect();
        if hits.len() != 1 {
            return Err(format!(
                "need exactly one symbol named {name}, found {}",
                hits.len()
            ));
        }
        let range = &hits[0]["range"];
        let text = read_text(&file, true)?;
        let start_position = field(range, "start")?;
        let end_position = field(range, "end")?;
        let start = offset(&text, start_position)?;
        let end = offset(&text, end_position)?;
        let chars: Vec<char> = text.chars().collect();
        let start = slice_index(&start, chars.len())?;
        let end = slice_index(&end, chars.len())?;
        let mut replacement: String = chars[..start.min(chars.len())].iter().collect();
        replacement.push_str(body);
        replacement.extend(chars[end.min(chars.len())..].iter());
        if cfg!(windows) {
            replacement = replacement.replace('\n', "\r\n");
        }
        std::fs::write(&file, replacement)
            .map_err(|error| crate::compat::file_error(&error, &file))?;
        let (key, _, _) = language(&file).ok_or_else(|| {
            format!(
                "'{}'",
                file.extension().unwrap_or_default().to_string_lossy()
            )
        })?;
        self.servers.remove(key);
        Ok(
            json!({"path":hits[0]["path"],"replaced":name,"lines":[add_one(field(start_position,"line")?)?,add_one(field(end_position,"line")?)?]}),
        )
    }
}

fn position(line: &BigInt, col: &BigInt) -> Value {
    let number = |value: &BigInt| bigint_value(value - BigInt::from(1));
    json!({"line":number(line),"character":number(col)})
}

fn tsserver_path() -> Option<String> {
    let home = crate::compat::home();
    [
        std::env::var_os("KIWI_TSSERVER_PATH").map(PathBuf::from),
        Some(
            home.join(".local/lib/kiwi-sprouts/typescript/node_modules/typescript/lib/tsserver.js"),
        ),
        Some(home.join(".local/lib/node_modules/typescript/lib/tsserver.js")),
    ]
    .into_iter()
    .flatten()
    .find(|path| path.is_file())
    .map(|path| path.to_string_lossy().to_string())
}

fn without_range(mut value: Value) -> Result<Value> {
    let kind = crate::protocol::type_name(&value);
    value
        .as_object_mut()
        .ok_or_else(|| format!("'{kind}' object has no attribute 'items'"))?
        .shift_remove("range");
    Ok(value)
}
fn as_list(value: Value) -> Vec<Value> {
    match value {
        Value::Array(list) => list,
        Value::Null => vec![],
        value if !crate::compat::truthy(&value) => vec![],
        value => vec![value],
    }
}
fn line_text(path: &Path, line: &Value) -> Result<String> {
    let text = read_text(path, false)?;
    let lines = splitlines(&text);
    if let Some(number) = float(line) {
        if !(number > 0.0 && number <= lines.len() as f64) {
            return Ok(String::new());
        }
    }
    let line = integer(line)?;
    Ok(line
        .to_usize()
        .filter(|line| *line > 0)
        .and_then(|line| lines.get(line - 1))
        .map_or("", |line| line.trim())
        .into())
}
#[derive(Eq, Hash, PartialEq)]
enum Coordinate {
    Integer(BigInt),
    Float(u64),
    Nan,
}

fn coordinate(value: &Value) -> Option<Coordinate> {
    if let Ok(integer) = integer(value) {
        return Some(Coordinate::Integer(integer));
    }
    let number = float(value)?;
    if number.is_nan() {
        Some(Coordinate::Nan)
    } else if number.fract() == 0.0 {
        BigInt::from_f64(number).map(Coordinate::Integer)
    } else {
        Some(Coordinate::Float(number.to_bits()))
    }
}

fn position_key(line: &Value, col: &Value) -> Option<(Coordinate, Coordinate)> {
    Some((coordinate(line)?, coordinate(col)?))
}

fn positions(values: &[Value]) -> Result<HashSet<(Coordinate, Coordinate)>> {
    values
        .iter()
        .map(|value| Ok(position_key(field(value, "line")?, field(value, "col")?)))
        .collect::<Result<Vec<_>>>()
        .map(|values| values.into_iter().flatten().collect())
}
fn slice_integer(value: &Value) -> Result<BigInt> {
    integer(value).map_err(|_| {
        "slice indices must be integers or None or have an __index__ method".to_string()
    })
}

fn slice_index(value: &Value, length: usize) -> Result<usize> {
    let offset = slice_integer(value)?;
    let length = BigInt::from(length);
    Ok(if offset < BigInt::from(0) {
        (length + offset).max(BigInt::from(0))
    } else {
        offset.min(length)
    }
    .to_usize()
    .unwrap_or(usize::MAX))
}

fn offset(text: &str, position: &Value) -> Result<Value> {
    let lines = splitlines(text);
    let line = field(position, "line")?;
    let count = if line.is_null() {
        lines.len()
    } else {
        slice_index(line, lines.len())?
    };
    let prefix = text
        .split_inclusive([
            '\n', '\r', '\u{000b}', '\u{000c}', '\u{001c}', '\u{001d}', '\u{001e}', '\u{0085}',
            '\u{2028}', '\u{2029}',
        ])
        .take(count)
        .map(|line| line.chars().count())
        .sum::<usize>();
    let character = field(position, "character")?;
    if let Ok(character) = integer(character) {
        return Ok(bigint_value(BigInt::from(prefix) + character));
    }
    if let Some(character) = float(character) {
        return Ok(crate::compat::float_value(prefix as f64 + character));
    }
    Err(format!(
        "unsupported operand type(s) for +: 'int' and '{}'",
        crate::protocol::type_name(character)
    ))
}

fn bigint_value(value: BigInt) -> Value {
    Value::Number(serde_json::Number::from_string_unchecked(value.to_string()))
}

fn integer(value: &Value) -> Result<BigInt> {
    value
        .as_bool()
        .map(|value| BigInt::from(u8::from(value)))
        .or_else(|| {
            value
                .as_number()
                .and_then(|number| number.to_string().parse().ok())
        })
        .ok_or_else(|| {
            format!(
                "list indices must be integers or slices, not {}",
                crate::protocol::type_name(value)
            )
        })
}

fn add_one(value: &Value) -> Result<Value> {
    if let Ok(value) = integer(value) {
        return Ok(bigint_value(value + BigInt::from(1)));
    }
    if let Some(value) = float(value) {
        return Ok(crate::compat::float_value(value + 1.0));
    }
    let kind = crate::protocol::type_name(value);
    Err(if matches!(value, Value::String(_) | Value::Array(_)) {
        format!("can only concatenate {kind} (not \"int\") to {kind}")
    } else {
        format!("unsupported operand type(s) for +: '{kind}' and 'int'")
    })
}

fn float(value: &Value) -> Option<f64> {
    value.as_number()?.to_string().parse().ok()
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a Value> {
    value
        .as_object()
        .ok_or_else(|| match value {
            Value::String(_) => "string indices must be integers, not 'str'".into(),
            Value::Array(_) => "list indices must be integers or slices, not str".into(),
            _ => format!(
                "'{}' object is not subscriptable",
                crate::protocol::type_name(value)
            ),
        })?
        .get(key)
        .ok_or_else(|| format!("'{key}'"))
}

fn symbol_name(symbol: &Value) -> Result<&str> {
    let value = field(symbol, "name")?;
    value.as_str().ok_or_else(|| {
        format!(
            "'{}' object has no attribute 'rsplit'",
            crate::protocol::type_name(value)
        )
    })
}

fn flatten(
    values: &Value,
    path: &Path,
    root: &Path,
    parent: &str,
    out: &mut Vec<Value>,
) -> Result<()> {
    let Some(values) = values.as_array() else {
        if !crate::compat::truthy(values) {
            return Ok(());
        }
        return Err(match values {
            Value::String(_) | Value::Object(_) => {
                "string indices must be integers, not 'str'".into()
            }
            _ => format!(
                "'{}' object is not iterable",
                crate::protocol::type_name(values)
            ),
        });
    };
    for symbol in values {
        let raw_name = field(symbol, "name")?;
        let swift = path.extension().is_some_and(|ext| ext == "swift");
        let mut name = raw_name
            .as_str()
            .ok_or_else(|| {
                let kind = crate::protocol::type_name(raw_name);
                if swift {
                    format!("'{kind}' object has no attribute 'endswith'")
                } else {
                    format!("can only concatenate str (not \"{kind}\") to str")
                }
            })?
            .to_string();
        if swift && name.ends_with(')') && name.contains('(') {
            name = name
                .split_once('(')
                .map_or(name.as_str(), |(name, _)| name)
                .into();
            if name.ends_with('>') && name.contains('<') {
                name = name
                    .split_once('<')
                    .map_or(name.as_str(), |(name, _)| name)
                    .into();
            }
        }
        if !parent.is_empty() {
            name = format!("{parent}/{name}");
        }
        let range = match symbol
            .get("range")
            .filter(|value| crate::compat::truthy(value))
        {
            Some(range) => range,
            None => field(field(symbol, "location")?, "range")?,
        };
        let selected = symbol.get("selectionRange").unwrap_or(range);
        let raw_kind = field(symbol, "kind")?;
        if matches!(raw_kind, Value::Array(_) | Value::Object(_)) {
            return Err(format!(
                "unhashable type: '{}'",
                crate::protocol::type_name(raw_kind)
            ));
        }
        let kind = match raw_kind.as_i64() {
            Some(5) => json!("class"),
            Some(6) => json!("method"),
            Some(11) => json!("interface"),
            Some(12) => json!("function"),
            Some(13) => json!("variable"),
            Some(14) => json!("constant"),
            Some(23) => json!("struct"),
            Some(10) => json!("enum"),
            Some(22) => json!("enum_member"),
            Some(8) => json!("field"),
            Some(2) => json!("module"),
            Some(9) => json!("constructor"),
            Some(26) => json!("type_param"),
            Some(7) => json!("property"),
            _ => raw_kind.clone(),
        };
        let start = field(selected, "start")?;
        let mut flat = json!({"name":name,"kind":null,"path":relative_required(path,root)?.to_string_lossy(),"line":null,"col":null,"range":null});
        flat["kind"] = kind;
        flat["line"] = add_one(field(start, "line")?)?;
        flat["col"] = add_one(field(start, "character")?)?;
        flat["range"] = range.clone();
        out.push(flat);
        flatten(&symbol["children"], path, root, &name, out)?;
    }
    Ok(())
}
