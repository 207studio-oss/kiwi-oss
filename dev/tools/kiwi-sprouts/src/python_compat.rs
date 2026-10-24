use serde_json::{json, Value};
use std::io::Write;
use std::process::Stdio;

fn call(operation: &str, fields: Value) -> Result<Value, String> {
    let python = crate::process::python()?;
    let mut child = crate::process::command(&python)
        .args(["-c", include_str!("python_compat.py")])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let input = child.stdin.take();
    crate::process::child_pipe(input, &mut child, "write")?
        .write_all(
            crate::compat::dumps(&json!({"operation":operation,"fields":fields}), None).as_bytes(),
        )
        .map_err(|error| error.to_string())?;
    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    let reply: Value = serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    if let Some(error) = reply["error"].as_str() {
        Err(error.into())
    } else {
        Ok(reply["result"].clone())
    }
}

pub fn error(operation: &str, fields: Value) -> String {
    call(operation, fields).err().unwrap_or_default()
}

pub fn parse(text: &str) -> Result<Value, String> {
    call("parse", json!({"text":text})).and_then(|tree| value(&tree))
}

fn value(tree: &Value) -> Result<Value, String> {
    if let Some(number) = tree["float"].as_str() {
        number
            .parse::<f64>()
            .map_err(|_| "invalid Python compatibility response".to_string())?;
        return Ok(Value::Number(serde_json::Number::from_string_unchecked(
            number.into(),
        )));
    }
    if let Some(items) = tree["dict"].as_array() {
        return Ok(Value::Object(
            items
                .iter()
                .map(|pair| {
                    let pair = pair
                        .as_array()
                        .filter(|pair| pair.len() == 2)
                        .ok_or("invalid Python compatibility response")?;
                    let key = pair[0]
                        .as_str()
                        .ok_or("invalid Python compatibility response")?;
                    Ok((key.into(), value(&pair[1])?))
                })
                .collect::<Result<_, String>>()?,
        ));
    }
    if let Some(items) = tree["list"].as_array() {
        return Ok(Value::Array(
            items.iter().map(value).collect::<Result<_, _>>()?,
        ));
    }
    tree.get("value")
        .cloned()
        .ok_or("invalid Python compatibility response".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_child_json_tree_is_an_error() {
        for tree in [
            json!({"dict":[[42,{"value":0}]]}),
            json!({"dict":[[]]}),
            json!({"float":"invalid.e"}),
            json!({}),
        ] {
            assert_eq!(
                value(&tree),
                Err("invalid Python compatibility response".into())
            );
        }
    }
}
