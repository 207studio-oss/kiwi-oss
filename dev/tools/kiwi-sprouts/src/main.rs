use kiwi_sprouts::{compat, process, protocol, Sprouts};
use std::path::Path;

fn health() -> compat::Result<i32> {
    let Ok(python) = process::python() else {
        return Ok(2);
    };
    let binary = std::env::current_exe().map_err(|error| error.to_string())?;
    Ok(process::command(&python)
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("kiwi_sprouts.py"))
        .arg("--health")
        .env("KIWI_SPROUTS_BIN", binary)
        .stdin(std::process::Stdio::inherit())
        .status()
        .ok()
        .and_then(|status| status.code())
        .unwrap_or(1))
}

fn run() -> compat::Result<i32> {
    let mut args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--health") {
        return health();
    }
    let mode = if args.first().is_some_and(|arg| arg == "--root") && args.len() >= 2 {
        &args[2..]
    } else {
        &args[..]
    };
    if !mode.is_empty() && mode[0] != "mcp" {
        return process::python_cli(&args);
    }
    let root = if args.first().is_some_and(|arg| arg == "--root") {
        if args.len() < 2 {
            return Err("list index out of range".into());
        }
        let root = args[1].clone();
        args.drain(..2);
        Some(root)
    } else {
        None
    };
    let mut sprouts = Sprouts::new(root.as_deref().map(Path::new))?;
    let code = protocol::mcp(
        &mut sprouts,
        std::io::stdin().lock(),
        std::io::stdout().lock(),
    )?;
    Ok(code)
}

fn main() {
    std::process::exit(match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    });
}
