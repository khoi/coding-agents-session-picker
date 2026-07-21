use std::fmt;

use clap::ValueEnum;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Agent {
    ClaudeCode,
    Codex,
    Cursor,
    Pi,
}

impl fmt::Display for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.to_possible_value().expect("no skipped variants").get_name())
    }
}

#[derive(Serialize)]
pub struct Session {
    pub agent: Agent,
    pub id: String,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub branch: Option<String>,
    pub created_at: jiff::Timestamp,
    pub updated_at: jiff::Timestamp,
    pub path: Option<String>,
}

pub fn sort_desc(sessions: &mut [Session]) {
    sessions.sort_unstable_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.agent.cmp(&b.agent))
            .then_with(|| a.id.cmp(&b.id))
    });
}

pub fn none_if_empty(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

pub fn truncate_chars(text: &str, max: usize) -> String {
    let mut chars = text.char_indices();
    match chars.nth(max) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(agent: Agent, id: &str, at: &str) -> Session {
        Session {
            agent,
            id: id.into(),
            title: None,
            cwd: None,
            branch: None,
            created_at: at.parse().unwrap(),
            updated_at: at.parse().unwrap(),
            path: None,
        }
    }

    #[test]
    fn sorts_updated_desc_then_agent_then_id() {
        let mut list = vec![
            session(Agent::Pi, "b", "2026-01-01T00:00:00Z"),
            session(Agent::Codex, "a", "2026-01-02T00:00:00Z"),
            session(Agent::Pi, "a", "2026-01-01T00:00:00Z"),
            session(Agent::ClaudeCode, "z", "2026-01-01T00:00:00Z"),
        ];
        sort_desc(&mut list);
        let keys: Vec<_> = list.iter().map(|s| (s.agent, s.id.as_str())).collect();
        assert_eq!(
            keys,
            [
                (Agent::Codex, "a"),
                (Agent::ClaudeCode, "z"),
                (Agent::Pi, "a"),
                (Agent::Pi, "b"),
            ]
        );
    }

    #[test]
    fn agent_serializes_kebab_case() {
        assert_eq!(serde_json::to_string(&Agent::ClaudeCode).unwrap(), "\"claude-code\"");
        assert_eq!(Agent::ClaudeCode.to_string(), "claude-code");
    }

    #[test]
    fn none_if_empty_normalizes() {
        assert_eq!(none_if_empty(Some("  ".into())), None);
        assert_eq!(none_if_empty(Some(" x ".into())), Some("x".into()));
        assert_eq!(none_if_empty(None), None);
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        assert_eq!(truncate_chars("héllo", 10), "héllo");
        assert_eq!(truncate_chars("héllo", 3), "hél…");
        assert_eq!(truncate_chars("日本語です", 2), "日本…");
    }
}
