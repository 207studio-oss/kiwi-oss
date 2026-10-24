use serde_json::Value;
use std::path::{Component, Path, PathBuf};

pub type Result<T> = std::result::Result<T, String>;

pub fn dumps(value: &Value, indent: Option<usize>) -> String {
    fn encode(value: &Value, indent: Option<usize>, depth: usize) -> String {
        let (open, close, values): (&str, &str, Vec<String>) = match value {
            Value::Array(items) => (
                "[",
                "]",
                items
                    .iter()
                    .map(|item| encode(item, indent, depth + 1))
                    .collect(),
            ),
            Value::Object(items) => (
                "{",
                "}",
                items
                    .iter()
                    .map(|(key, item)| {
                        format!(
                            "{}: {}",
                            serde_json::to_string(key).unwrap(),
                            encode(item, indent, depth + 1)
                        )
                    })
                    .collect(),
            ),
            Value::Number(number) => return number_text(number),
            _ => return serde_json::to_string(value).unwrap(),
        };
        if values.is_empty() {
            return format!("{open}{close}");
        }
        match indent {
            Some(n) => {
                let pad = " ".repeat((depth + 1) * n);
                format!(
                    "{open}\n{pad}{}\n{}{close}",
                    values.join(&format!(",\n{pad}")),
                    " ".repeat(depth * n)
                )
            }
            None => format!("{open}{}{close}", values.join(", ")),
        }
    }
    encode(value, indent, 0)
}

fn number_text(number: &serde_json::Number) -> String {
    let text = number.to_string();
    if !text.contains(['.', 'e', 'E']) {
        return text
            .parse::<num_bigint::BigInt>()
            .map(|number| number.to_string())
            .unwrap_or(text);
    }
    let float = text.parse::<f64>().unwrap();
    if float.is_infinite() {
        return if float.is_sign_negative() {
            "-Infinity"
        } else {
            "Infinity"
        }
        .into();
    }
    float_text(float)
}

pub fn float_value(float: f64) -> Value {
    let text = if float.is_nan() {
        "NaN".into()
    } else if float.is_infinite() {
        if float.is_sign_negative() {
            "-Infinity".into()
        } else {
            "Infinity".into()
        }
    } else {
        float_text(float)
    };
    Value::Number(serde_json::Number::from_string_unchecked(text))
}

fn float_text(float: f64) -> String {
    if float == 0.0 {
        return if float.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .into();
    }
    let mut buffer = ryu::Buffer::new();
    let text = buffer.format_finite(float.abs());
    let (mantissa, exponent) = text
        .split_once('e')
        .map_or((text, 0), |(mantissa, exponent)| {
            (mantissa, exponent.parse::<i32>().unwrap())
        });
    let decimal = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
    let raw: String = mantissa.chars().filter(|ch| *ch != '.').collect();
    let leading = raw.len() - raw.trim_start_matches('0').len();
    let digits = raw.trim_start_matches('0').trim_end_matches('0');
    let exponent = exponent + decimal - 1 - leading as i32;
    let sign = if float.is_sign_negative() { "-" } else { "" };
    if !(-4..16).contains(&exponent) {
        let rest = if digits.len() > 1 {
            format!(".{}", &digits[1..])
        } else {
            String::new()
        };
        format!(
            "{sign}{}{rest}e{}{:02}",
            &digits[..1],
            if exponent < 0 { "-" } else { "+" },
            exponent.abs()
        )
    } else if exponent < 0 {
        format!("{sign}0.{}{digits}", "0".repeat((-exponent - 1) as usize))
    } else {
        let position = exponent as usize + 1;
        if position >= digits.len() {
            format!("{sign}{digits}{}.0", "0".repeat(position - digits.len()))
        } else {
            format!("{sign}{}.{}", &digits[..position], &digits[position..])
        }
    }
}

pub fn joined(root: &Path, path: &str) -> PathBuf {
    #[cfg(windows)]
    {
        let mut components = Path::new(path).components();
        if let Some(Component::Prefix(prefix)) = components.next() {
            if let std::path::Prefix::Disk(drive) = prefix.kind() {
                let mut root_components = root.components();
                if let Some(Component::Prefix(root_prefix)) = root_components.next() {
                    if matches!(root_prefix.kind(),std::path::Prefix::Disk(root_drive) if root_drive.eq_ignore_ascii_case(&drive))
                        && !matches!(components.clone().next(), Some(Component::RootDir))
                    {
                        return root.join(components.collect::<PathBuf>());
                    }
                }
            }
        }
    }
    root.join(path)
}

pub fn output_line(value: &str) -> String {
    if cfg!(windows) {
        format!("{}\r\n", value.replace('\n', "\r\n"))
    } else {
        format!("{value}\n")
    }
}

pub fn dumps_ascii(value: &Value) -> String {
    let mut output = String::new();
    for ch in dumps(value, None).chars() {
        if ch.is_ascii() {
            output.push(ch);
        } else {
            let mut units = [0; 2];
            for unit in ch.encode_utf16(&mut units) {
                output.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    output
}

pub fn text_encoding() -> &'static encoding_rs::Encoding {
    if std::env::var("PYTHONUTF8").as_deref() == Ok("1") {
        return encoding_rs::UTF_8;
    }
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetACP() -> u32;
        }
        let code = unsafe { GetACP() };
        let label = match code {
            949 => "euc-kr".into(),
            932 => "shift_jis".into(),
            936 => "gbk".into(),
            950 => "big5".into(),
            65001 => "utf-8".into(),
            value => format!("windows-{value}"),
        };
        encoding_rs::Encoding::for_label(label.as_bytes()).unwrap_or(encoding_rs::UTF_8)
    }
    #[cfg(not(windows))]
    {
        encoding_rs::UTF_8
    }
}

pub(crate) fn stdio_encoding() -> (String, &'static encoding_rs::Encoding) {
    if let Ok(setting) = std::env::var("PYTHONIOENCODING") {
        let label = setting.split(':').next().unwrap_or("");
        if !label.is_empty() {
            return (
                label.into(),
                if label.eq_ignore_ascii_case("cp949") {
                    encoding_rs::EUC_KR
                } else {
                    encoding_rs::Encoding::for_label(label.as_bytes()).unwrap_or(encoding_rs::UTF_8)
                },
            );
        }
    }
    let encoding = text_encoding();
    (
        if encoding == encoding_rs::EUC_KR {
            "cp949".into()
        } else {
            encoding.name().into()
        },
        encoding,
    )
}

pub fn write_stdio(output: &mut impl std::io::Write, text: &str) -> Result<()> {
    let text = output_line(text);
    let (label, encoding) = stdio_encoding();
    let (bytes, _, invalid) = encoding.encode(&text);
    if invalid || (label.eq_ignore_ascii_case("ascii") && !text.is_ascii()) {
        return Err(crate::python_compat::error(
            "encode",
            serde_json::json!({"text":text,"encoding":label}),
        ));
    }
    output.write_all(&bytes).map_err(|error| error.to_string())
}

pub fn decode_stdio(bytes: &[u8]) -> Result<String> {
    let (label, encoding) = stdio_encoding();
    if let Some(text) = encoding
        .decode_without_bom_handling_and_without_replacement(bytes)
        .filter(|text| !label.eq_ignore_ascii_case("ascii") || text.is_ascii())
    {
        return Ok(text.into_owned());
    }
    Err(crate::python_compat::error(
        "decode",
        serde_json::json!({"bytes":bytes,"encoding":label}),
    ))
}

pub fn pystr(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        _ => dumps(value, None),
    }
}

pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
        Value::Number(value) => value.as_f64() != Some(0.0),
    }
}

pub fn home() -> PathBuf {
    #[cfg(windows)]
    let key = "USERPROFILE";
    #[cfg(not(windows))]
    let key = "HOME";
    std::env::var_os(key)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn current_dir() -> Result<PathBuf> {
    std::env::current_dir().map_err(|error| {
        #[cfg(windows)]
        {
            let code = error.raw_os_error().unwrap_or(0);
            let text = error.to_string();
            format!(
                "[WinError {code}] {}",
                text.strip_suffix(&format!(" (os error {code})"))
                    .unwrap_or(&text)
            )
        }
        #[cfg(not(windows))]
        {
            let text = file_error(&error, Path::new(""));
            text.strip_suffix(": ''").unwrap_or(&text).to_string()
        }
    })
}

pub fn resolved(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        current_dir()?.join(path)
    };
    let mut result = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            other => result.push(other.as_os_str()),
        }
        if let Ok(real) = dunce::canonicalize(&result) {
            result = real;
        }
    }
    Ok(result)
}

pub fn relative_required(path: &Path, root: &Path) -> Result<PathBuf> {
    path.strip_prefix(root).map(Path::to_path_buf).map_err(|_| {
        format!(
            "'{}' is not in the subpath of '{}'",
            path.display(),
            root.display()
        )
    })
}

pub fn relative(path: &Path, root: &Path) -> Option<PathBuf> {
    if !cfg!(windows) {
        return path.strip_prefix(root).ok().map(Path::to_path_buf);
    }
    let mut parts = path.components();
    for expected in root.components() {
        if parts.next()?.as_os_str().to_string_lossy().to_lowercase()
            != expected.as_os_str().to_string_lossy().to_lowercase()
        {
            return None;
        }
    }
    Some(parts.collect())
}

pub fn denied(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == ".git")
}

pub fn line_denied(line: &str) -> bool {
    line.split(|c: char| c.is_whitespace() || c == ':')
        .any(|token| (token.contains('/') || token.contains('.')) && denied(Path::new(token)))
}

pub fn tail(text: &str, size: usize) -> String {
    text.chars()
        .skip(text.chars().count().saturating_sub(size))
        .collect()
}

pub fn head(text: &str, size: usize) -> String {
    text.chars().take(size).collect()
}

pub fn read_text(path: &Path, strict: bool) -> Result<String> {
    let bytes = std::fs::read(path).map_err(|error| file_error(&error, path))?;
    let text = if strict {
        String::from_utf8(bytes).map_err(|error| error.to_string())?
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };
    Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
}

pub fn splitlines(text: &str) -> Vec<&str> {
    let mut lines: Vec<_> = text
        .split([
            '\n', '\r', '\u{000b}', '\u{000c}', '\u{001c}', '\u{001d}', '\u{001e}', '\u{0085}',
            '\u{2028}', '\u{2029}',
        ])
        .collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

pub fn file_error(error: &std::io::Error, path: &Path) -> String {
    let message = match error.kind() {
        std::io::ErrorKind::NotFound => "No such file or directory",
        std::io::ErrorKind::PermissionDenied => "Permission denied",
        std::io::ErrorKind::IsADirectory => "Is a directory",
        _ => return error.to_string(),
    };
    let code = match error.kind() {
        std::io::ErrorKind::NotFound => 2,
        std::io::ErrorKind::PermissionDenied => 13,
        _ => 21,
    };
    format!(
        "[Errno {code}] {message}: '{}'",
        path.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('\'', "\\'")
    )
}
