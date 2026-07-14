use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

use anyhow::Context;
use serde_json::Value;

use crate::session::{Agent, Session, truncate_chars};

const WINDOW: u64 = 2 * 1024 * 1024;
const MESSAGE_LIMIT: usize = 6;
const MESSAGE_CHARS: usize = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Speaker {
    User,
    Agent,
}

impl Speaker {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Message {
    pub(crate) speaker: Speaker,
    pub(crate) text: String,
}

struct Node {
    parent: Option<String>,
    message: Option<Message>,
    order: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Graph {
    Claude,
    Pi,
}

pub(crate) fn load(session: &Session) -> anyhow::Result<Vec<Message>> {
    let path = session
        .path
        .as_deref()
        .context("conversation unavailable")?;
    let entries = entries(Path::new(path))?;
    let messages = match session.agent {
        Agent::ClaudeCode => graph_messages(Graph::Claude, &entries),
        Agent::Codex => codex_messages(&entries),
        Agent::Cursor => entries.iter().filter_map(cursor_message).collect(),
        Agent::Pi => graph_messages(Graph::Pi, &entries),
    };
    Ok(recent_messages(messages))
}

fn entries(path: &Path) -> anyhow::Result<Vec<Value>> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(WINDOW);
    file.seek(SeekFrom::Start(start))?;
    let mut reader = BufReader::new(file);
    if start > 0 {
        let mut partial = Vec::new();
        reader.read_until(b'\n', &mut partial)?;
    }
    let mut entries = Vec::new();
    for line in reader.lines() {
        if let Ok(entry) = serde_json::from_str(&line?) {
            entries.push(entry);
        }
    }
    Ok(entries)
}

fn graph_messages(graph: Graph, entries: &[Value]) -> Vec<Message> {
    let (id_key, parent_key) = match graph {
        Graph::Claude => ("uuid", "parentUuid"),
        Graph::Pi => ("id", "parentId"),
    };
    let mut nodes = HashMap::new();
    let mut leaf = None;
    for (order, entry) in entries.iter().enumerate() {
        if graph == Graph::Claude && entry["isSidechain"] == true {
            continue;
        }
        let Some(id) = entry[id_key].as_str() else {
            continue;
        };
        leaf = Some(id.to_owned());
        nodes.insert(
            id.to_owned(),
            Node {
                parent: entry[parent_key].as_str().map(str::to_owned),
                message: graph_message(graph, entry),
                order,
            },
        );
    }
    let mut current = match graph {
        Graph::Claude => newest_terminal_message(&nodes),
        Graph::Pi => leaf,
    };
    let mut seen = HashSet::new();
    let mut messages = Vec::new();
    while let Some(id) = current {
        if !seen.insert(id.clone()) {
            break;
        }
        let Some(node) = nodes.get(&id) else {
            break;
        };
        if let Some(message) = &node.message {
            messages.push(message.clone());
        }
        current = node.parent.clone();
    }
    messages.reverse();
    messages
}

fn newest_terminal_message(nodes: &HashMap<String, Node>) -> Option<String> {
    let parents: HashSet<_> = nodes
        .values()
        .filter_map(|node| node.parent.as_deref())
        .collect();
    nodes
        .keys()
        .filter(|id| !parents.contains(id.as_str()))
        .filter_map(|id| {
            let mut current = Some(id.as_str());
            let mut seen = HashSet::new();
            while let Some(id) = current {
                if !seen.insert(id) {
                    return None;
                }
                let node = nodes.get(id)?;
                if node.message.is_some() {
                    return Some((node.order, id.to_owned()));
                }
                current = node.parent.as_deref();
            }
            None
        })
        .max_by_key(|(order, _)| *order)
        .map(|(_, id)| id)
}

fn codex_messages(entries: &[Value]) -> Vec<Message> {
    let mut turns: Vec<Vec<Message>> = Vec::new();
    let mut current = Vec::new();
    let mut active = false;
    let mut explicit = false;
    for entry in entries {
        let event = (entry["type"] == "event_msg")
            .then(|| entry["payload"]["type"].as_str())
            .flatten();
        match event {
            Some("task_started" | "turn_started") => {
                finish_turn(&mut turns, &mut current, &mut active, &mut explicit);
                active = true;
                explicit = true;
            }
            Some("task_complete" | "turn_complete" | "turn_aborted") => {
                finish_turn(&mut turns, &mut current, &mut active, &mut explicit);
            }
            Some("thread_rolled_back") => {
                finish_turn(&mut turns, &mut current, &mut active, &mut explicit);
                let count =
                    usize::try_from(entry["payload"]["num_turns"].as_u64().unwrap_or_default())
                        .unwrap_or(usize::MAX);
                let keep = turns.len().saturating_sub(count);
                turns.truncate(keep);
            }
            _ => {
                let Some(message) = codex_message(entry) else {
                    continue;
                };
                if message.speaker == Speaker::User
                    && active
                    && !explicit
                    && current
                        .iter()
                        .any(|message| message.speaker == Speaker::User)
                {
                    finish_turn(&mut turns, &mut current, &mut active, &mut explicit);
                }
                active = true;
                current.push(message);
            }
        }
    }
    finish_turn(&mut turns, &mut current, &mut active, &mut explicit);
    turns.into_iter().flatten().collect()
}

fn finish_turn(
    turns: &mut Vec<Vec<Message>>,
    current: &mut Vec<Message>,
    active: &mut bool,
    explicit: &mut bool,
) {
    if *active {
        turns.push(std::mem::take(current));
    }
    *active = false;
    *explicit = false;
}

fn graph_message(graph: Graph, entry: &Value) -> Option<Message> {
    match graph {
        Graph::Claude => {
            if entry["isMeta"] == true || entry["isCompactSummary"] == true {
                return None;
            }
            nested_message(entry["type"].as_str()?, &entry["message"])
        }
        Graph::Pi => {
            if entry["type"] != "message" {
                return None;
            }
            nested_message(entry["message"]["role"].as_str()?, &entry["message"])
        }
    }
}

fn cursor_message(entry: &Value) -> Option<Message> {
    nested_message(entry["role"].as_str()?, &entry["message"])
}

fn codex_message(entry: &Value) -> Option<Message> {
    if entry["type"] != "event_msg" {
        return None;
    }
    let payload = &entry["payload"];
    match payload["type"].as_str()? {
        "user_message" => visible(Speaker::User, payload["message"].as_str()?.to_owned()),
        "agent_message" => visible(Speaker::Agent, payload["message"].as_str()?.to_owned()),
        "item_completed" => {
            let item = &payload["item"];
            let speaker = match item["type"].as_str()? {
                "UserMessage" => Speaker::User,
                "AgentMessage" => Speaker::Agent,
                _ => return None,
            };
            visible(speaker, text_blocks(&item["content"]).join("\n"))
        }
        _ => None,
    }
}

fn nested_message(role: &str, message: &Value) -> Option<Message> {
    let speaker = match role {
        "user" => Speaker::User,
        "assistant" => Speaker::Agent,
        _ => return None,
    };
    visible(speaker, text_blocks(&message["content"]).join("\n"))
}

fn visible(speaker: Speaker, text: String) -> Option<Message> {
    let text = text.trim();
    if text.is_empty()
        || speaker == Speaker::User
            && (is_preamble(text) || text.starts_with("# AGENTS.md instructions"))
    {
        return None;
    }
    Some(Message {
        speaker,
        text: text.to_owned(),
    })
}

fn recent_messages(messages: Vec<Message>) -> Vec<Message> {
    let mut messages: Vec<_> = messages
        .into_iter()
        .filter_map(|mut message| {
            let text = message
                .text
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if text.is_empty() {
                return None;
            }
            message.text = truncate_chars(&text, MESSAGE_CHARS);
            Some(message)
        })
        .collect();
    messages.dedup();
    let start = messages.len().saturating_sub(MESSAGE_LIMIT);
    let mut recent = messages.split_off(start);
    if !recent
        .iter()
        .any(|message| message.speaker == Speaker::User)
        && let Some(user) = messages
            .into_iter()
            .rfind(|message| message.speaker == Speaker::User)
        && let Some(first) = recent.first_mut()
    {
        *first = user;
    }
    recent
}

pub(crate) fn text_blocks(content: &Value) -> Vec<String> {
    match content {
        Value::String(text) => vec![text.clone()],
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| {
                matches!(
                    block["type"].as_str(),
                    Some("text" | "input_text" | "output_text" | "Text")
                )
            })
            .filter_map(|block| block["text"].as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

pub(crate) fn is_preamble(text: &str) -> bool {
    if text.starts_with("[Request interrupted by user") {
        return true;
    }
    let mut chars = text.chars();
    if chars.next() != Some('<') || !chars.next().is_some_and(|c| c.is_ascii_lowercase()) {
        return false;
    }
    chars
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '-'))
        .is_some_and(|c| c == '>' || c.is_whitespace())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn session(agent: Agent, path: &Path) -> Session {
        Session {
            agent,
            id: "id".into(),
            title: None,
            cwd: None,
            branch: None,
            updated_at: "2026-07-01T00:00:00Z".parse().unwrap(),
            path: Some(path.to_string_lossy().into_owned()),
        }
    }

    fn write(lines: &[&str]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(&path, lines.join("\n")).unwrap();
        (dir, path)
    }

    fn texts(messages: &[Message]) -> Vec<&str> {
        messages
            .iter()
            .map(|message| message.text.as_str())
            .collect()
    }

    #[test]
    fn claude_follows_most_recent_terminal_branch() {
        let (_dir, path) = write(&[
            r#"{"type":"user","uuid":"one","parentUuid":null,"message":{"content":"start"}}"#,
            r#"{"type":"user","uuid":"stale","parentUuid":"one","message":{"content":"abandoned"}}"#,
            r#"{"type":"assistant","uuid":"two","parentUuid":"one","message":{"content":[{"type":"text","text":"kept"}]}}"#,
            r#"{"type":"last-prompt","sessionId":"session","lastPrompt":"start"}"#,
        ]);
        assert_eq!(
            texts(&load(&session(Agent::ClaudeCode, &path)).unwrap()),
            ["start", "kept"]
        );
    }

    #[test]
    fn pi_follows_latest_leaf_through_invisible_nodes() {
        let (_dir, path) = write(&[
            r#"{"type":"session","id":"root","cwd":"/w"}"#,
            r#"{"type":"message","id":"one","parentId":"root","message":{"role":"user","content":"start"}}"#,
            r#"{"type":"message","id":"two","parentId":"one","message":{"role":"assistant","content":[{"type":"text","text":"kept"}]}}"#,
            r#"{"type":"message","id":"stale","parentId":"one","message":{"role":"user","content":"abandoned"}}"#,
            r#"{"type":"model_change","id":"leaf","parentId":"two"}"#,
        ]);
        assert_eq!(
            texts(&load(&session(Agent::Pi, &path)).unwrap()),
            ["start", "kept"]
        );
    }

    #[test]
    fn codex_reads_message_events_and_applies_rollbacks() {
        let (_dir, path) = write(&[
            r#"{"type":"event_msg","payload":{"type":"turn_started"}}"#,
            r#"{"type":"event_msg","payload":{"type":"user_message","message":"hello"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"duplicate"}]}}"#,
            r#"{"type":"event_msg","payload":{"type":"agent_message","message":"answer"}}"#,
            r#"{"type":"event_msg","payload":{"type":"user_message","message":"steer"}}"#,
            r#"{"type":"event_msg","payload":{"type":"agent_message","message":"steered answer"}}"#,
            r#"{"type":"event_msg","payload":{"type":"turn_complete"}}"#,
            r#"{"type":"event_msg","payload":{"type":"turn_started"}}"#,
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[{"type":"text","text":"discarded"}]}}}"#,
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","content":[{"type":"Text","text":"discarded answer"}]}}}"#,
            r#"{"type":"event_msg","payload":{"type":"turn_complete"}}"#,
            r#"{"type":"event_msg","payload":{"type":"thread_rolled_back","num_turns":1}}"#,
            r#"{"type":"event_msg","payload":{"type":"turn_started"}}"#,
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[{"type":"text","text":"next"}]}}}"#,
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","content":[{"type":"Text","text":"done"}]}}}"#,
            r#"{"type":"event_msg","payload":{"type":"turn_complete"}}"#,
        ]);
        assert_eq!(
            texts(&load(&session(Agent::Codex, &path)).unwrap()),
            ["hello", "answer", "steer", "steered answer", "next", "done"]
        );
    }

    #[test]
    fn cursor_reads_user_and_agent_text_only() {
        let (_dir, path) = write(&[
            r#"{"role":"user","message":{"content":[{"type":"text","text":"hello"}]}}"#,
            r#"{"role":"assistant","message":{"content":[{"type":"tool_use","name":"x"},{"type":"text","text":"answer"}]}}"#,
            r#"{"type":"turn_ended"}"#,
        ]);
        assert_eq!(
            texts(&load(&session(Agent::Cursor, &path)).unwrap()),
            ["hello", "answer"]
        );
    }

    #[test]
    fn keeps_recent_messages_and_latest_user_context() {
        let messages = vec![
            Message {
                speaker: Speaker::User,
                text: "prompt".into(),
            },
            Message {
                speaker: Speaker::Agent,
                text: "one".into(),
            },
            Message {
                speaker: Speaker::Agent,
                text: "two".into(),
            },
            Message {
                speaker: Speaker::Agent,
                text: "three".into(),
            },
            Message {
                speaker: Speaker::Agent,
                text: "four".into(),
            },
            Message {
                speaker: Speaker::Agent,
                text: "five".into(),
            },
            Message {
                speaker: Speaker::Agent,
                text: "six\ncontinued".into(),
            },
        ];
        assert_eq!(
            texts(&recent_messages(messages)),
            ["prompt", "two", "three", "four", "five", "six continued"]
        );
    }
}
