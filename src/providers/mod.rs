use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use crate::cache::Cache;
use crate::session::{Agent, Session};

mod claude;
mod codex;
mod cursor;
mod pi;

pub trait Provider: Send + Sync {
    fn agent(&self) -> Agent;
    fn sessions(&self, cache: &Cache) -> anyhow::Result<Vec<Session>>;
}

pub fn all(root: Option<&Path>, include_archived: bool) -> Vec<Box<dyn Provider>> {
    let base = root
        .map(Path::to_path_buf)
        .or_else(std::env::home_dir)
        .unwrap_or_default();
    let claude_home = override_dir(root, "CLAUDE_CONFIG_DIR").unwrap_or_else(|| base.join(".claude"));
    let codex_home = override_dir(root, "CODEX_HOME").unwrap_or_else(|| base.join(".codex"));
    vec![
        Box::new(claude::Claude {
            projects: claude_home.join("projects"),
        }),
        Box::new(codex::Codex {
            home: codex_home,
            include_archived,
        }),
        Box::new(cursor::Cursor {
            db: base.join("Library/Application Support/Cursor/User/globalStorage/state.vscdb"),
            projects: base.join(".cursor/projects"),
        }),
        Box::new(pi::Pi {
            sessions: base.join(".pi/agent/sessions"),
        }),
    ]
}

fn override_dir(root: Option<&Path>, env: &str) -> Option<PathBuf> {
    if root.is_some() {
        return None;
    }
    std::env::var_os(env).map(PathBuf::from)
}

fn jsonl_files(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if dir.exists() {
        collect_jsonl(dir, &mut files)?;
    }
    Ok(files)
}

fn collect_jsonl(dir: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            collect_jsonl(&path, files)?;
        } else if path.extension().is_some_and(|ext| ext == "jsonl") {
            files.push(path);
        }
    }
    Ok(())
}

fn modified_at(path: &Path) -> Option<jiff::Timestamp> {
    let timestamp: jiff::Timestamp = fs::metadata(path).ok()?.modified().ok()?.try_into().ok()?;
    timestamp
        .round(jiff::TimestampRound::new().smallest(jiff::Unit::Millisecond))
        .ok()
}
