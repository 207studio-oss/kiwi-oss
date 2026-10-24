#![allow(dead_code)]
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Env { pub home: PathBuf }
impl Env {
    pub fn new(name: &str) -> Env {
        let home = std::env::temp_dir().join(format!("kiwi-test-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        Env { home }
    }
    pub fn run_in(&self, dir: Option<&Path>, args: &[&str]) -> (i32, Value, String) {
        let mut c = Command::new(env!("CARGO_BIN_EXE_kiwi"));
        c.args(args).env("KIWI_HOME", &self.home).env_remove("OPENAI_API_KEY").env_remove("KIWI_DB").env("KIWI_AUTOREFRESH", "off").env("KIWI_SEMANTIC", "off").env("KIWI_DECISIONS", "off").env("KIWI_CONTEXT_ANSWER", "off");
        if let Some(d) = dir { c.current_dir(d); }
        let o = c.output().unwrap();
        let out = String::from_utf8_lossy(&o.stdout).to_string();
        (o.status.code().unwrap_or(-1), serde_json::from_str(&out).unwrap_or(Value::Null), String::from_utf8_lossy(&o.stderr).to_string())
    }
    pub fn run(&self, args: &[&str]) -> (i32, Value, String) { self.run_in(None, args) }
}
