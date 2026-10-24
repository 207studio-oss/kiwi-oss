use kiwi_sprouts::{compat, protocol, Sprouts};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::Path;

fn main() {
    let root = std::env::args().nth(1);
    let mut sprouts = match Sprouts::new(root.as_deref().map(Path::new)) {
        Ok(sprouts) => sprouts,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        let request: Value = serde_json::from_str(&line).unwrap();
        let result = match request["name"].as_str().unwrap() {
            "abs" => sprouts
                .abs(request["args"]["path"].as_str().unwrap())
                .map(|path| json!(path)),
            "files" => sprouts
                .files(request["args"]["path"].as_str().unwrap())
                .map(|paths| json!(paths)),
            "denied" => Ok(json!(compat::denied(Path::new(
                request["args"]["path"].as_str().unwrap()
            )))),
            "resources" => sprouts.rule_resources(),
            name => protocol::call(&mut sprouts, name, &request["args"]),
        };
        let reply = match result {
            Ok(value) => json!({"result":value}),
            Err(error) => json!({"error":error}),
        };
        println!("{}", compat::dumps(&reply, None));
        std::io::stdout().flush().unwrap();
    }
}
