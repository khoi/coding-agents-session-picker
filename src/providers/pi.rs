use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde::Deserialize;
use serde_json::Value;

use crate::session::{Agent, Session, none_if_empty, truncate_chars};

pub struct Pi {
    pub sessions: PathBuf,
}

#[derive(Deserialize)]
struct Header {
    id: String,
    cwd: String,
}

impl super::Provider for Pi {
    fn agent(&self) -> Agent {
        Agent::Pi
    }

    fn sessions(&self) -> anyhow::Result<Vec<Session>> {
        let files = super::jsonl_files(&self.sessions)?;
        Ok(files.par_iter().filter_map(|path| read_session(path)).collect())
    }
}

fn read_session(path: &Path) -> Option<Session> {
    let updated_at = super::modified_at(path)?;
    let mut bytes = Vec::new();
    File::open(path).ok()?.take(64 * 1024).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text.lines();
    let header: Header = serde_json::from_str(lines.next()?).ok()?;
    let title = none_if_empty(
        lines
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|line| line["type"] == "message" && line["message"]["role"] == "user")
            .map(|entry| super::text_blocks(&entry["message"]["content"]).join(" ")),
    )
    .map(|text| truncate_chars(&text, 80));
    Some(Session {
        agent: Agent::Pi,
        id: header.id,
        title,
        cwd: Some(header.cwd),
        branch: None,
        updated_at,
        path: Some(path.to_string_lossy().into_owned()),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn write_session(dir: &Path, name: &str, lines: &[&str]) -> PathBuf {
        let project = dir.join("--Users-x-proj--");
        fs::create_dir_all(&project).unwrap();
        let path = project.join(name);
        fs::write(&path, lines.join("\n")).unwrap();
        path
    }

    #[test]
    fn reads_header_and_first_user_message() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_session(
            dir.path(),
            "a.jsonl",
            &[
                r#"{"type":"session","version":3,"id":"019e-1","timestamp":"2026-05-11T15:02:58.512Z","cwd":"/Users/x/proj"}"#,
                r#"{"type":"model_change","provider":"anthropic"}"#,
                r#"{"type":"message","message":{"role":"user","content":[{"type":"text","text":"fix the login bug"}]}}"#,
            ],
        );
        let session = read_session(&path).unwrap();
        assert_eq!(session.id, "019e-1");
        assert_eq!(session.cwd.as_deref(), Some("/Users/x/proj"));
        assert_eq!(session.title.as_deref(), Some("fix the login bug"));
        assert_eq!(session.branch, None);
    }

    #[test]
    fn truncates_title_on_char_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let long = "日".repeat(100);
        let path = write_session(
            dir.path(),
            "b.jsonl",
            &[
                r#"{"type":"session","id":"019e-2","cwd":"/w"}"#,
                &format!(r#"{{"type":"message","message":{{"role":"user","content":"{long}"}}}}"#),
            ],
        );
        let title = read_session(&path).unwrap().title.unwrap();
        assert_eq!(title.chars().count(), 81);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn header_only_session_has_null_title() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_session(dir.path(), "c.jsonl", &[r#"{"type":"session","id":"019e-3","cwd":"/w"}"#]);
        assert_eq!(read_session(&path).unwrap().title, None);
    }

    #[test]
    fn malformed_header_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_session(dir.path(), "d.jsonl", &["not json"]);
        assert!(read_session(&path).is_none());
    }
}
