use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde_json::Value;

use crate::scrape::{extract_first, extract_last};
use crate::session::{Agent, Session, none_if_empty, sort_desc, truncate_chars};

pub struct Claude {
    pub projects: PathBuf,
}

const WINDOW: u64 = 64 * 1024;

impl super::Provider for Claude {
    fn agent(&self) -> Agent {
        Agent::ClaudeCode
    }

    fn sessions(&self) -> anyhow::Result<Vec<Session>> {
        let files = super::jsonl_files(&self.projects)?;
        let mut sessions: Vec<Session> = files.par_iter().filter_map(|path| read_session(path)).collect();
        sort_desc(&mut sessions);
        let mut seen = HashSet::new();
        sessions.retain(|session| seen.insert(session.id.clone()));
        Ok(sessions)
    }
}

fn read_session(path: &Path) -> Option<Session> {
    let id = path.file_stem()?.to_str()?;
    if !is_uuid(id) {
        return None;
    }
    let len = fs::metadata(path).ok()?.len();
    if len == 0 {
        return None;
    }
    let (head, tail) = head_tail(path, len).ok()?;
    let first_line = head.lines().next().unwrap_or_default();
    if first_line.contains("\"isSidechain\":true") || first_line.contains("\"isSidechain\": true") {
        return None;
    }
    Some(Session {
        agent: Agent::ClaudeCode,
        id: id.to_owned(),
        title: Some(title(&head, &tail)?),
        cwd: none_if_empty(extract_first(&head, "cwd")),
        branch: none_if_empty(extract_last(&tail, "gitBranch").or_else(|| extract_first(&head, "gitBranch"))),
        updated_at: super::modified_at(path)?,
        path: Some(path.to_string_lossy().into_owned()),
    })
}

fn head_tail(path: &Path, len: u64) -> std::io::Result<(String, String)> {
    let mut file = File::open(path)?;
    let mut head = vec![0u8; WINDOW.min(len) as usize];
    file.read_exact(&mut head)?;
    let head = String::from_utf8_lossy(&head).into_owned();
    if len <= WINDOW {
        return Ok((head.clone(), head));
    }
    let mut tail = vec![0u8; WINDOW as usize];
    file.seek(SeekFrom::End(-(WINDOW as i64)))?;
    file.read_exact(&mut tail)?;
    Ok((head, String::from_utf8_lossy(&tail).into_owned()))
}

fn title(head: &str, tail: &str) -> Option<String> {
    none_if_empty(extract_last(tail, "customTitle").or_else(|| extract_last(head, "customTitle")))
        .or_else(|| none_if_empty(extract_last(tail, "aiTitle").or_else(|| extract_last(head, "aiTitle"))))
        .or_else(|| none_if_empty(extract_last(tail, "lastPrompt")))
        .or_else(|| none_if_empty(extract_last(tail, "summary")))
        .or_else(|| first_prompt(head))
}

fn first_prompt(head: &str) -> Option<String> {
    let mut command = None;
    let prompt = head
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|entry| {
            entry["type"] == "user" && entry["isMeta"] != true && entry["isCompactSummary"] != true
        })
        .flat_map(|entry| super::text_blocks(&entry["message"]["content"]))
        .find_map(|text| {
            if command.is_none() {
                command = command_text(&text);
            }
            prompt_text(&text)
        });
    prompt.or(command).map(|text| truncate_chars(&text, 200))
}

fn command_text(text: &str) -> Option<String> {
    let name = text.split_once("<command-name>")?.1.split("</command-name>").next()?.trim();
    if name.is_empty() {
        return None;
    }
    let args = text
        .split_once("<command-args>")
        .and_then(|(_, rest)| rest.split("</command-args>").next())
        .unwrap_or_default()
        .trim();
    Some(if args.is_empty() { name.to_owned() } else { format!("{name} {args}") })
}

fn prompt_text(text: &str) -> Option<String> {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix("<bash-input>") {
        let command = rest.split("</bash-input>").next().unwrap_or(rest).trim();
        return (!command.is_empty()).then(|| format!("! {command}"));
    }
    if text.is_empty() || super::is_preamble(text) {
        return None;
    }
    Some(text.to_owned())
}

fn is_uuid(text: &str) -> bool {
    text.len() == 36
        && text.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0199aaaa-bbbb-cccc-dddd-eeeeffff0000";

    fn user_line(text: &str) -> String {
        format!(
            r#"{{"type":"user","sessionId":"{ID}","cwd":"/w","gitBranch":"main","timestamp":"2026-07-01T00:00:00Z","message":{{"role":"user","content":[{{"type":"text","text":{}}}]}}}}"#,
            serde_json::to_string(text).unwrap()
        )
    }

    fn write_session(dir: &Path, lines: &[String]) -> PathBuf {
        let path = dir.join(format!("{ID}.jsonl"));
        fs::write(&path, lines.join("\n")).unwrap();
        path
    }

    #[test]
    fn title_chain_precedence() {
        let queue = r#"{"type":"queue-operation","sessionId":"x"}"#.to_owned();
        let user = user_line("first prompt here");
        let ai = format!(r#"{{"type":"ai-title","aiTitle":"ai title","sessionId":"{ID}"}}"#);
        let custom = format!(r#"{{"type":"custom-title","customTitle":"my title","sessionId":"{ID}"}}"#);
        let last = format!(r#"{{"type":"last-prompt","lastPrompt":"latest ask","sessionId":"{ID}"}}"#);
        let dir = tempfile::tempdir().unwrap();

        let all = write_session(dir.path(), &[queue.clone(), user.clone(), ai.clone(), last.clone(), custom.clone()]);
        assert_eq!(read_session(&all).unwrap().title.as_deref(), Some("my title"));
        fs::remove_file(&all).unwrap();

        let no_custom = write_session(dir.path(), &[queue.clone(), user.clone(), last.clone(), ai.clone()]);
        assert_eq!(read_session(&no_custom).unwrap().title.as_deref(), Some("ai title"));
        fs::remove_file(&no_custom).unwrap();

        let no_ai = write_session(dir.path(), &[queue.clone(), user.clone(), last]);
        assert_eq!(read_session(&no_ai).unwrap().title.as_deref(), Some("latest ask"));
        fs::remove_file(&no_ai).unwrap();

        let prompt_only = write_session(dir.path(), &[queue, user]);
        assert_eq!(read_session(&prompt_only).unwrap().title.as_deref(), Some("first prompt here"));
    }

    #[test]
    fn skips_sidechains_metadata_only_and_non_uuid_files() {
        let dir = tempfile::tempdir().unwrap();
        let sidechain = write_session(
            dir.path(),
            &[r#"{"type":"user","isSidechain":true,"cwd":"/w","message":{"content":"hi"}}"#.to_string()],
        );
        assert!(read_session(&sidechain).is_none());

        let queue_only = write_session(dir.path(), &[r#"{"type":"queue-operation","sessionId":"x"}"#.to_owned()]);
        assert!(read_session(&queue_only).is_none());

        let named = dir.path().join("notes.jsonl");
        fs::write(&named, user_line("hello")).unwrap();
        assert!(read_session(&named).is_none());

        let empty = dir.path().join(format!("{}.jsonl", ID.replace('0', "1")));
        fs::write(&empty, "").unwrap();
        assert!(read_session(&empty).is_none());
    }

    #[test]
    fn first_prompt_skips_preambles_and_renders_bash_input() {
        let dir = tempfile::tempdir().unwrap();
        let lines = [
            user_line("<ide_selection>src/main.rs</ide_selection>"),
            user_line("[Request interrupted by user]"),
            user_line("<bash-input>cargo test</bash-input>"),
        ];
        let path = write_session(dir.path(), &lines);
        assert_eq!(read_session(&path).unwrap().title.as_deref(), Some("! cargo test"));
    }

    #[test]
    fn slash_command_is_title_fallback_only() {
        let dir = tempfile::tempdir().unwrap();
        let command = user_line("<command-name>/review</command-name><command-args>src/main.rs</command-args>");
        let command_only = write_session(dir.path(), std::slice::from_ref(&command));
        assert_eq!(read_session(&command_only).unwrap().title.as_deref(), Some("/review src/main.rs"));
        fs::remove_file(&command_only).unwrap();

        let with_prompt = write_session(dir.path(), &[command, user_line("real prompt wins")]);
        assert_eq!(read_session(&with_prompt).unwrap().title.as_deref(), Some("real prompt wins"));
    }

    #[test]
    fn survives_truncated_oversized_first_line() {
        let dir = tempfile::tempdir().unwrap();
        let huge = format!(
            r#"{{"type":"user","cwd":"/big/project","gitBranch":"feat","message":{{"content":"{}"}}}}"#,
            "x".repeat(2 * WINDOW as usize)
        );
        let lines = [huge, r#"{"type":"ai-title","aiTitle":"survivor"}"#.to_string()];
        let path = write_session(dir.path(), &lines);
        let session = read_session(&path).unwrap();
        assert_eq!(session.cwd.as_deref(), Some("/big/project"));
        assert_eq!(session.branch.as_deref(), Some("feat"));
        assert_eq!(session.title.as_deref(), Some("survivor"));
    }

    #[test]
    fn empty_branch_normalizes_to_none() {
        let dir = tempfile::tempdir().unwrap();
        let line = r#"{"type":"user","cwd":"/w","gitBranch":"","message":{"content":"hello"}}"#.to_owned();
        let path = write_session(dir.path(), &[line]);
        assert_eq!(read_session(&path).unwrap().branch, None);
    }
}
