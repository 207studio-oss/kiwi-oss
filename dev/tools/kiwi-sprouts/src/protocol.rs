use crate::compat::{dumps, pystr, truthy, Result};
use crate::Sprouts;
use num_bigint::BigInt;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

const INSTRUCTIONS: &str = "Kiwi: Decisions is handled by Kiwi; never ask the user for an API key. Call context_for once at task start and follow user_intents, mandatory_rules, then selected_context. Open omitted_refs as kiwi://rule/{id} resources.";

pub fn tools() -> Value {
    serde_json::from_str(include_str!("tools.json")).unwrap()
}

pub fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
        Value::Number(number) if number.is_f64() => "float",
        Value::Number(_) => "int",
    }
}

pub(crate) fn validate(name: &str, args: &Value) -> Result<()> {
    let tools = tools();
    let tool = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == name)
        .ok_or_else(|| format!("unknown tool {name}"))?;
    let mapping = args.as_object().ok_or_else(|| {
        format!(
            "__main__.Sprouts.{name}() argument after ** must be a mapping, not {}",
            type_name(args)
        )
    })?;
    let properties = tool["inputSchema"]["properties"].as_object().unwrap();
    for key in mapping.keys() {
        if !properties.contains_key(key) {
            return Err(format!(
                "Sprouts.{name}() got an unexpected keyword argument '{key}'"
            ));
        }
    }
    let missing: Vec<_> = tool["inputSchema"]["required"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .filter(|key| !mapping.contains_key(*key))
        .collect();
    if !missing.is_empty() {
        let quoted: Vec<_> = missing.iter().map(|key| format!("'{key}'")).collect();
        let names = match quoted.len() {
            1 => quoted[0].clone(),
            2 => quoted.join(" and "),
            _ => format!(
                "{}, and {}",
                quoted[..quoted.len() - 1].join(", "),
                quoted.last().unwrap()
            ),
        };
        return Err(format!(
            "Sprouts.{name}() missing {} required positional argument{}: {names}",
            missing.len(),
            if missing.len() == 1 { "" } else { "s" }
        ));
    }
    Ok(())
}

pub fn call(sp: &mut Sprouts, name: &str, args: &Value) -> Result<Value> {
    validate(name, args)?;
    let text = |key: &str| -> Result<&str> {
        args[key].as_str().ok_or_else(|| {
            format!(
                "expected str, bytes or os.PathLike object, not {}",
                type_name(&args[key])
            )
        })
    };
    let integer = |key: &str| -> Result<BigInt> {
        args[key]
            .as_bool()
            .map(|value| BigInt::from(u8::from(value)))
            .or_else(|| {
                args[key]
                    .as_number()
                    .and_then(|number| number.to_string().parse().ok())
            })
            .ok_or_else(|| {
                format!(
                    "unsupported operand type(s) for -: '{}' and 'int'",
                    type_name(&args[key])
                )
            })
    };
    let path = args.get("path").map_or(Ok("."), |_| text("path"));
    match name {
        "code_overview" => sp.code_overview(text("path")?),
        "code_find" => sp.code_find(text("name")?, path?),
        "code_definition" => sp.code_definition(text("path")?, &integer("line")?, &integer("col")?),
        "code_references" => sp.code_references(text("path")?, &integer("line")?, &integer("col")?),
        "code_replace_body" => sp.code_replace_body(text("path")?, text("name")?, text("body")?),
        "ops_graph" => {
            let values = args["args"]
                .as_array()
                .ok_or("ops_graph allows: rules, trace, owners, preflight, validate")?;
            sp.ops_graph(&values.iter().map(pystr).collect::<Vec<_>>())
        }
        "context_for" => sp.context_for(args),
        "kiwi_rule" => sp.kiwi_rule(text("id")?),
        "kiwi_status" => sp.kiwi(&["status".into()]),
        "task_guard" => sp.kiwi(&[
            "task".into(),
            "guard".into(),
            "--task-id".into(),
            pystr(&args["task_id"]),
            "--fencing-token".into(),
            pystr(&args["fencing_token"]),
        ]),
        "docs_search" => sp.docs_search(text("query")?, args.get("k").unwrap_or(&json!(5))),
        "query" => sp.query(
            text("q")?,
            path?,
            args.get("subject")
                .filter(|value| truthy(value))
                .and_then(Value::as_str),
        ),
        "decide" => sp.decide(args),
        "ask" | "ask_resolve" => sp.ask(name, args),
        "ask_stats" => sp.ask_stats(args),
        _ => unreachable!(),
    }
}

fn get<'a>(value: &'a Value, key: &str) -> Result<&'a Value> {
    value.get(key).ok_or_else(|| format!("'{key}'"))
}

fn rpc_reply(id: Value, key: &str, value: Value) -> Value {
    let mut reply = json!({"jsonrpc":"2.0","id":null});
    reply["id"] = id;
    reply[key] = value;
    reply
}

pub fn respond(sp: &mut Sprouts, message: Value) -> Option<Value> {
    let id = message.get("id").filter(|value| !value.is_null())?.clone();
    let method = message.get("method").and_then(Value::as_str);
    let result = (|| -> Result<Value> {
        match method {
            Some("initialize") => {
                let params = get(&message, "params")?;
                if !params.is_object() {
                    return Err(format!(
                        "'{}' object has no attribute 'get'",
                        type_name(params)
                    ));
                }
                let mut result = json!({"protocolVersion":null,"capabilities":{"tools":{},"resources":{}},"instructions":INSTRUCTIONS,"serverInfo":{"name":"kiwi-sprouts","version":"0.1.0"}});
                result["protocolVersion"] = params
                    .get("protocolVersion")
                    .cloned()
                    .unwrap_or(json!("2025-06-18"));
                Ok(result)
            }
            Some("tools/list") => Ok(json!({"tools":tools()})),
            Some("tools/call") => {
                let outcome = (|| {
                    let params = get(&message, "params")?;
                    let name = get(params, "name")?;
                    let name = name.as_str().ok_or_else(|| {
                        let kind = type_name(name);
                        if matches!(name, Value::Array(_) | Value::Object(_)) {
                            format!("unhashable type: '{kind}'")
                        } else {
                            format!("can only concatenate str (not \"{kind}\") to str")
                        }
                    })?;
                    let empty = json!({});
                    let args = params
                        .get("arguments")
                        .filter(|value| truthy(value))
                        .unwrap_or(&empty);
                    call(sp, name, args)
                })();
                Ok(match outcome {
                    Ok(value) => json!({"content":[{"type":"text","text":dumps(&value,None)}]}),
                    Err(error) => {
                        json!({"content":[{"type":"text","text":format!("error: {error}")}],"isError":true})
                    }
                })
            }
            Some("resources/list") => Ok(json!({"resources":sp.rule_resources()?})),
            Some("resources/templates/list") => Ok(
                json!({"resourceTemplates":[{"uriTemplate":"kiwi://rule/{id}","name":"Kiwi rule","mimeType":"text/markdown"}]}),
            ),
            Some("resources/read") => sp.read_resource(
                get(get(&message, "params")?, "uri")?
                    .as_str()
                    .ok_or("'NoneType' object has no attribute 'startswith'")?,
            ),
            Some("ping") => Ok(json!({})),
            _ => Ok(Value::Null),
        }
    })();
    Some(
        if !matches!(
            method,
            Some(
                "initialize"
                    | "tools/list"
                    | "tools/call"
                    | "resources/list"
                    | "resources/templates/list"
                    | "resources/read"
                    | "ping"
            )
        ) {
            let mut error = json!({"code":-32601,"message":null});
            error["message"] = message.get("method").cloned().unwrap_or(Value::Null);
            rpc_reply(id, "error", error)
        } else {
            match result {
                Ok(value) => rpc_reply(id, "result", value),
                Err(error) => rpc_reply(id, "error", json!({"code":-32000,"message":error})),
            }
        },
    )
}

pub fn mcp(sp: &mut Sprouts, mut input: impl BufRead, mut output: impl Write) -> Result<i32> {
    let mut codec = crate::stdio_codec::StdioCodec::new();
    let mut pending = String::new();
    loop {
        let mut bytes = vec![];
        let buffer = input.fill_buf().map_err(|error| error.to_string())?;
        bytes.extend_from_slice(buffer);
        input.consume(bytes.len());
        let eof = bytes.is_empty();
        pending.push_str(&codec.decode(&bytes, eof)?);
        while let Some(end) = pending
            .find('\n')
            .map(|end| end + 1)
            .or_else(|| (eof && !pending.is_empty()).then_some(pending.len()))
        {
            let line: String = pending.drain(..end).collect();
            if line
                .trim_matches(|ch: char| {
                    ch.is_whitespace() || matches!(ch, '\u{001c}'..='\u{001f}')
                })
                .is_empty()
            {
                continue;
            }
            let message = match parse_json(&line) {
                Ok(message) if message.is_object() => message,
                _ => return crate::process::report_mcp_error(&sp.root, &line),
            };
            if let Some(reply) = respond(sp, message) {
                if let Some(code) = sp.decisions.fatal_code {
                    return Ok(code);
                }
                let text = if reply.get("error").is_some() {
                    crate::compat::dumps_ascii(&reply)
                } else {
                    dumps(&reply, None)
                };
                if let Err(error) = codec.write(&mut output, &text) {
                    let reply = rpc_reply(
                        reply["id"].clone(),
                        "error",
                        json!({"code":-32000,"message":error}),
                    );
                    codec.write(&mut output, &crate::compat::dumps_ascii(&reply))?;
                }
                output.flush().map_err(|error| error.to_string())?;
            }
        }
        if eof {
            break;
        }
    }
    Ok(0)
}

pub fn parse_json(text: &str) -> Result<Value> {
    serde_json::from_str(text).or_else(|_| crate::python_compat::parse(text))
}
