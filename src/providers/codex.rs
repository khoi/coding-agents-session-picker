use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use rayon::prelude::*;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use crate::cache::Cache;
use crate::conversation::{is_preamble, text_blocks};
use crate::session::{Agent, Session, none_if_empty, truncate_chars};

pub struct Codex {
    pub home: PathBuf,
    pub include_archived: bool,
}

const MS_EPOCH_2020: i64 = 1_577_836_800_000;

impl super::Provider for Codex {
    fn agent(&self) -> Agent {
        Agent::Codex
    }

    fn sessions(&self, cache: &Cache) -> anyhow::Result<Vec<Session>> {
        let titles = index_titles(&self.home.join("session_index.jsonl"));
        let db = self.home.join("state_5.sqlite");
        if db.exists() {
            match query_threads(&db, self.include_archived, &titles, cache) {
                Ok(sessions) => return Ok(sessions),
                Err(err) => eprintln!("{}: codex: {err:#}; falling back to rollout scan", env!("CARGO_BIN_NAME")),
            }
        }
        self.scan_rollouts(&titles, cache)
    }
}

fn index_titles(path: &Path) -> HashMap<String, String> {
    let Ok(text) = fs::read_to_string(path) else {
        return HashMap::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|entry| {
            Some((
                entry["id"].as_str()?.to_owned(),
                none_if_empty(entry["thread_name"].as_str().map(str::to_owned))?,
            ))
        })
        .collect()
}

fn query_threads(
    db: &Path,
    include_archived: bool,
    titles: &HashMap<String, String>,
    cache: &Cache,
) -> anyhow::Result<Vec<Session>> {
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening {}", db.display()))?;
    conn.busy_timeout(Duration::from_secs(1))?;
    let mut sql = String::from(
        "SELECT id, rollout_path, cwd, title, first_user_message, preview, git_branch, created_at, updated_at, created_at_ms, updated_at_ms FROM threads",
    );
    if !include_archived {
        sql.push_str(" WHERE archived = 0");
    }
    let mut statement = conn.prepare(&sql).context("querying threads")?;
    let mut sessions: Vec<Session> = statement
        .query_map([], |row| {
            let id: String = row.get(0)?;
            let title = none_if_empty(row.get(3)?)
                .or_else(|| titles.get(&id).cloned())
                .or_else(|| none_if_empty(row.get(4).unwrap_or(None)))
                .or_else(|| none_if_empty(row.get(5).unwrap_or(None)))
                .map(|text| truncate_chars(&text, 200));
            Ok((
                id,
                row.get::<_, Option<String>>(1)?,
                none_if_empty(row.get(2)?),
                title,
                none_if_empty(row.get(6)?),
                to_timestamp(row.get(9)?, row.get(7)?),
                to_timestamp(row.get(10)?, row.get(8)?),
            ))
        })
        .context("querying threads")?
        .filter_map(Result::ok)
        .filter_map(|(id, path, cwd, title, branch, created_at, updated_at)| {
            Some(Session {
                agent: Agent::Codex,
                id,
                title,
                cwd,
                branch,
                created_at: created_at?,
                updated_at: updated_at?,
                path,
            })
        })
        .collect();
    sessions
        .par_iter_mut()
        .filter(|session| session.title.is_none())
        .for_each(|session| {
            session.title = session
                .path
                .as_deref()
                .and_then(|path| cache.title(Path::new(path), first_user_prompt));
        });
    Ok(sessions)
}

fn first_user_prompt(path: &Path) -> Option<String> {
    let reader = BufReader::new(File::open(path).ok()?);
    reader
        .lines()
        .take(200)
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .find_map(|line| line_user_prompt(&line))
}

fn line_user_prompt(line: &Value) -> Option<String> {
    let payload = &line["payload"];
    let texts = if line["type"] == "event_msg" && payload["type"] == "user_message" {
        vec![payload["message"].as_str()?.to_owned()]
    } else if line["type"] == "response_item" && payload["type"] == "message" && payload["role"] == "user" {
        text_blocks(&payload["content"])
    } else {
        return None;
    };
    texts
        .iter()
        .map(|text| text.trim())
        .find(|text| {
            !text.is_empty()
                && !is_preamble(text)
                && !text.starts_with("# AGENTS.md instructions")
        })
        .map(|text| truncate_chars(text, 200))
}

fn to_timestamp(ms: Option<i64>, seconds: Option<i64>) -> Option<jiff::Timestamp> {
    let raw = ms.filter(|v| *v > 0).or(seconds.filter(|v| *v > 0))?;
    let ms = if raw < MS_EPOCH_2020 { raw * 1000 } else { raw };
    jiff::Timestamp::from_millisecond(ms).ok()
}

impl Codex {
    fn scan_rollouts(&self, titles: &HashMap<String, String>, cache: &Cache) -> anyhow::Result<Vec<Session>> {
        let mut files = super::jsonl_files(&self.home.join("sessions"))?;
        if self.include_archived {
            files.extend(super::jsonl_files(&self.home.join("archived_sessions"))?);
        }
        let mut sessions = cache.sessions(files, read_rollout);
        for session in &mut sessions {
            if let Some(name) = titles.get(&session.id) {
                session.title = Some(name.clone());
            }
        }
        Ok(sessions)
    }
}

fn read_rollout(path: &Path) -> Option<Session> {
    let updated_at = super::modified_at(path)?;
    let mut meta = None;
    let mut created_at = None;
    let mut prompt = None;
    for (index, line) in BufReader::new(File::open(path).ok()?)
        .lines()
        .take(200)
        .map_while(Result::ok)
        .enumerate()
    {
        let Ok(line) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if meta.is_none() && line["type"] == "session_meta" {
            created_at = line["payload"]["timestamp"]
                .as_str()
                .or_else(|| line["timestamp"].as_str())
                .and_then(|timestamp| timestamp.parse().ok());
            meta = Some(line["payload"].clone());
        } else if prompt.is_none() {
            prompt = line_user_prompt(&line);
        }
        if meta.is_none() && index >= 9 {
            return None;
        }
        if meta.is_some() && prompt.is_some() {
            break;
        }
    }
    let payload = meta?;
    let id = payload["id"].as_str().or_else(|| payload["session_id"].as_str())?;
    Some(Session {
        agent: Agent::Codex,
        id: id.to_owned(),
        title: prompt,
        cwd: none_if_empty(payload["cwd"].as_str().map(str::to_owned)),
        branch: none_if_empty(payload["git"]["branch"].as_str().map(str::to_owned)),
        created_at: created_at?,
        updated_at,
        path: Some(path.to_string_lossy().into_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_guard_scales_second_precision() {
        assert_eq!(
            to_timestamp(Some(1_751_500_000), None).unwrap().to_string(),
            "2025-07-02T23:46:40Z"
        );
        assert_eq!(
            to_timestamp(Some(1_751_500_000_000), None).unwrap().to_string(),
            "2025-07-02T23:46:40Z"
        );
        assert_eq!(to_timestamp(None, Some(1_751_500_000)).unwrap().to_string(), "2025-07-02T23:46:40Z");
        assert_eq!(to_timestamp(None, None), None);
        assert_eq!(to_timestamp(Some(0), Some(0)), None);
    }

    #[test]
    fn index_titles_last_entry_wins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session_index.jsonl");
        fs::write(
            &path,
            concat!(
                "{\"id\":\"t1\",\"thread_name\":\"old name\",\"updated_at\":\"2026-01-01\"}\n",
                "garbage line\n",
                "{\"id\":\"t1\",\"thread_name\":\"new name\",\"updated_at\":\"2026-02-01\"}\n",
            ),
        )
        .unwrap();
        assert_eq!(index_titles(&path).get("t1").map(String::as_str), Some("new name"));
        assert!(index_titles(&dir.path().join("missing.jsonl")).is_empty());
    }

    #[test]
    fn rollout_meta_tolerates_session_id_only_and_missing_git() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout-2026-07-01T10-00-00-abc.jsonl");
        fs::write(
            &path,
            concat!(
                "{\"timestamp\":\"2026-07-01T10:01:00Z\",\"type\":\"session_meta\",",
                "\"payload\":{\"session_id\":\"legacy-id\",\"cwd\":\"/w\",\"timestamp\":\"2026-07-01T10:00:00Z\"}}\n",
            ),
        )
        .unwrap();
        let session = read_rollout(&path).unwrap();
        assert_eq!(session.id, "legacy-id");
        assert_eq!(session.cwd.as_deref(), Some("/w"));
        assert_eq!(session.branch, None);
        assert_eq!(session.title, None);
        assert_eq!(session.created_at.to_string(), "2026-07-01T10:00:00Z");
    }

    #[test]
    fn rollout_title_falls_back_to_first_user_prompt_past_preambles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout-2026-07-01T10-00-00-ghi.jsonl");
        fs::write(
            &path,
            concat!(
                "{\"timestamp\":\"2026-07-01T10:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"cx-p\",\"cwd\":\"/w\"}}\n",
                "{\"timestamp\":\"t\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"developer\",\"content\":[{\"type\":\"input_text\",\"text\":\"be careful\"}]}}\n",
                "{\"timestamp\":\"t\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"# AGENTS.md instructions for /w\\nrules\"}]}}\n",
                "{\"timestamp\":\"t\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"<environment_context>zsh</environment_context>\"}]}}\n",
                "{\"timestamp\":\"t\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"fix the sidebar\"}]}}\n",
            ),
        )
        .unwrap();
        let session = read_rollout(&path).unwrap();
        assert_eq!(session.title.as_deref(), Some("fix the sidebar"));
    }

    #[test]
    fn user_prompt_reads_event_msg_user_message() {
        let line: Value = serde_json::from_str(
            r#"{"timestamp":"t","type":"event_msg","payload":{"type":"user_message","message":"hello there"}}"#,
        )
        .unwrap();
        assert_eq!(line_user_prompt(&line).as_deref(), Some("hello there"));
        let preamble: Value = serde_json::from_str(
            r#"{"timestamp":"t","type":"event_msg","payload":{"type":"user_message","message":"<user_instructions>x</user_instructions>"}}"#,
        )
        .unwrap();
        assert_eq!(line_user_prompt(&preamble), None);
    }

    #[test]
    fn scan_overlays_index_titles_onto_rollout_meta() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions/rollout-2026-07-01T10-00-00-def.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            concat!(
                "{\"timestamp\":\"2026-07-01T10:00:00Z\",\"type\":\"event_msg\",\"payload\":{}}\n",
                "{\"timestamp\":\"2026-07-01T10:00:00Z\",\"type\":\"session_meta\",",
                "\"payload\":{\"id\":\"new-id\",\"cwd\":\"/w\",\"git\":{\"branch\":\"main\"}}}\n",
            ),
        )
        .unwrap();
        let codex = Codex {
            home: dir.path().to_path_buf(),
            include_archived: false,
        };
        let titles = HashMap::from([("new-id".to_owned(), "renamed".to_owned())]);
        let sessions = codex.scan_rollouts(&titles, &Cache::load(None)).unwrap();
        assert_eq!(sessions[0].id, "new-id");
        assert_eq!(sessions[0].branch.as_deref(), Some("main"));
        assert_eq!(sessions[0].title.as_deref(), Some("renamed"));
    }

    #[test]
    fn rollout_without_creation_time_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        fs::write(
            &path,
            r#"{"type":"session_meta","payload":{"id":"missing","cwd":"/w"}}"#,
        )
        .unwrap();
        assert!(read_rollout(&path).is_none());
    }
}
