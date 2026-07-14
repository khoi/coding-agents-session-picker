use std::io::Write;

use serde_json::Value;

use crate::session::Session;

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Format {
    Json,
    Ndjson,
    Table,
}

pub fn render(format: Format, sessions: &[Session], out: &mut impl Write) -> anyhow::Result<()> {
    match format {
        Format::Json => {
            serde_json::to_writer_pretty(&mut *out, sessions)?;
            writeln!(out)?;
        }
        Format::Ndjson => {
            for session in sessions {
                serde_json::to_writer(&mut *out, session)?;
                writeln!(out)?;
            }
        }
        Format::Table => {
            writeln!(out, "agent\tupdated_at\tid\tbranch\tcwd\ttitle")?;
            for session in sessions {
                let row = serde_json::to_value(session)?;
                let cells: Vec<_> = ["agent", "updated_at", "id", "branch", "cwd", "title"]
                    .map(|key| cell(&row[key]))
                    .into();
                writeln!(out, "{}", cells.join("\t"))?;
            }
        }
    }
    Ok(())
}

fn cell(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.replace(['\t', '\n', '\r'], " "),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Agent;

    fn sample() -> Vec<Session> {
        vec![Session {
            agent: Agent::Pi,
            id: "abc".into(),
            title: Some("line one\nline\ttwo".into()),
            cwd: Some("/w".into()),
            branch: None,
            created_at: "2026-01-01T01:02:03Z".parse().unwrap(),
            updated_at: "2026-01-02T03:04:05Z".parse().unwrap(),
            path: None,
        }]
    }

    fn rendered(format: Format) -> String {
        let mut buf = Vec::new();
        render(format, &sample(), &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn json_has_all_keys_with_explicit_nulls() {
        let value: Value = serde_json::from_str(&rendered(Format::Json)).unwrap();
        let object = value[0].as_object().unwrap();
        let keys: Vec<_> = object.keys().collect();
        assert_eq!(keys, ["agent", "branch", "created_at", "cwd", "id", "path", "title", "updated_at"]);
        assert_eq!(object["branch"], Value::Null);
        assert_eq!(object["created_at"], "2026-01-01T01:02:03Z");
        assert_eq!(object["updated_at"], "2026-01-02T03:04:05Z");
    }

    #[test]
    fn ndjson_is_one_compact_object_per_line() {
        let text = rendered(Format::Ndjson);
        assert_eq!(text.lines().count(), 1);
        assert!(serde_json::from_str::<Value>(text.trim()).is_ok());
    }

    #[test]
    fn table_strips_control_chars_and_blanks_nulls() {
        let text = rendered(Format::Table);
        let row = text.lines().nth(1).unwrap();
        assert_eq!(row, "pi\t2026-01-02T03:04:05Z\tabc\t\t/w\tline one line two");
    }
}
