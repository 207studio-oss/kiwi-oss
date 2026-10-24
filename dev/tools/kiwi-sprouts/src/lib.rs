mod backends;
mod code;
pub mod compat;
pub mod lsp;
pub mod process;
pub mod protocol;
mod python_compat;
mod stdio_codec;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct Sprouts {
    pub root: PathBuf,
    pub kiwi_root: PathBuf,
    servers: HashMap<String, lsp::Lsp>,
    decisions: process::JsonWorker,
}

impl Sprouts {
    pub fn new(root: Option<&Path>) -> compat::Result<Self> {
        let root = match root.filter(|path| !path.as_os_str().is_empty()) {
            Some(root) => root.to_path_buf(),
            None => {
                let cwd = compat::current_dir()?;
                std::env::var_os("KIWI_SPROUTS_ROOT")
                    .map(PathBuf::from)
                    .unwrap_or(cwd)
            }
        };
        let kiwi_root = match std::env::var_os("KIWI_ROOT") {
            Some(root) => PathBuf::from(root),
            None => Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(3)
                .ok_or("2")?
                .to_path_buf(),
        };
        Ok(Self {
            root: compat::resolved(&root)?,
            kiwi_root,
            servers: HashMap::new(),
            decisions: process::JsonWorker::new(include_str!("decisions.py")),
        })
    }
}
