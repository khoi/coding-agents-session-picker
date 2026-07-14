use std::fs;
use std::path::Path;
use std::process::Output;
use std::time::{Duration, UNIX_EPOCH};

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

const BASE_MS: i64 = 1_780_000_000_000;
const CLAUDE_A: &str = "11111111-1111-4111-8111-111111111111";
const CLAUDE_B: &str = "22222222-2222-4222-8222-222222222222";

fn run(home: &Path, args: &[&str]) -> Output {
    Command::cargo_bin("ap")
        .unwrap()
        .arg("--root")
        .arg(home)
        .args(args)
        .output()
        .unwrap()
}

fn stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("stdout must be valid JSON")
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn version_uses_binary_name() {
    let output = Command::cargo_bin("ap")
        .unwrap()
        .arg("--version")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("ap {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(stderr_text(&output), "");
}

fn write_at(path: &Path, content: &str, mtime_ms: i64) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_millis(mtime_ms as u64))
        .unwrap();
}

fn claude_fixture(root: &Path) {
    let user_a = format!(
        r#"{{"type":"user","sessionId":"{CLAUDE_A}","cwd":"/w/one","gitBranch":"main","timestamp":"2026-06-01T00:00:00Z","message":{{"role":"user","content":"start here"}}}}"#
    );
    write_at(
        &root.join(format!(".claude/projects/-w-one/{CLAUDE_A}.jsonl")),
        &format!(
            "{{\"type\":\"queue-operation\",\"sessionId\":\"{CLAUDE_A}\"}}\n{user_a}\n{{\"type\":\"ai-title\",\"aiTitle\":\"Claude session A\"}}"
        ),
        BASE_MS + 35_000,
    );
    let user_b = format!(
        r#"{{"type":"user","sessionId":"{CLAUDE_B}","cwd":"/w/one/sub","gitBranch":"","timestamp":"2026-06-02T00:00:00Z","message":{{"role":"user","content":"hello from B"}}}}"#
    );
    write_at(
        &root.join(format!(".claude/projects/-w-one-sub/{CLAUDE_B}.jsonl")),
        &user_b,
        BASE_MS + 25_000,
    );
}

fn pi_fixture(root: &Path) {
    write_at(
        &root.join(".pi/agent/sessions/--w-two--/2026-06-01T00-00-00-000Z_pi-1.jsonl"),
        concat!(
            "{\"type\":\"session\",\"version\":3,\"id\":\"pi-1\",\"timestamp\":\"2026-06-01T00:00:00Z\",\"cwd\":\"/w/two\"}\n",
            "{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"pi prompt\"}]}}",
        ),
        BASE_MS + 15_000,
    );
}

fn codex_db_fixture(root: &Path) {
    let path = root.join(".codex/state_5.sqlite");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "journal_mode", "wal").unwrap();
    conn.execute_batch(
        "CREATE TABLE threads (
            id TEXT PRIMARY KEY, rollout_path TEXT, cwd TEXT, title TEXT,
            first_user_message TEXT, preview TEXT, git_branch TEXT,
            created_at INTEGER, updated_at INTEGER, created_at_ms INTEGER, updated_at_ms INTEGER,
            archived INTEGER NOT NULL DEFAULT 0
        )",
    )
    .unwrap();
    let mut insert = conn
        .prepare(
            "INSERT INTO threads (id, rollout_path, cwd, title, first_user_message, preview, git_branch, created_at_ms, updated_at, updated_at_ms, archived)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )
        .unwrap();
    insert
        .execute(rusqlite::params![
            "cx-recent",
            "/r/recent.jsonl",
            "/w/one",
            "Fix tab groups",
            "",
            "",
            "main",
            BASE_MS + 10_000,
            (BASE_MS + 40_000) / 1000,
            BASE_MS + 40_000,
            0,
        ])
        .unwrap();
    insert
        .execute(rusqlite::params![
            "cx-seconds",
            "/r/seconds.jsonl",
            "/w/two",
            "",
            "explore the API",
            "",
            None::<String>,
            BASE_MS + 15_000,
            (BASE_MS + 30_000) / 1000,
            None::<i64>,
            0,
        ])
        .unwrap();
    insert
        .execute(rusqlite::params![
            "cx-archived",
            "/r/archived.jsonl",
            "/w/one",
            "Old thread",
            "",
            "",
            None::<String>,
            BASE_MS,
            (BASE_MS + 50_000) / 1000,
            BASE_MS + 50_000,
            1,
        ])
        .unwrap();
}

fn codex_rollout_fixture(root: &Path) {
    write_at(
        &root.join(".codex/sessions/2026/07/01/rollout-2026-07-01T10-00-00-cx-scan.jsonl"),
        concat!(
            "{\"timestamp\":\"2026-07-01T10:00:00Z\",\"type\":\"session_meta\",",
            "\"payload\":{\"id\":\"cx-scan\",\"cwd\":\"/w/three\",\"timestamp\":\"2026-07-01T09:59:00Z\",\"git\":{\"branch\":\"feat\"}}}\n",
            "{\"timestamp\":\"2026-07-01T10:00:01Z\",\"type\":\"response_item\",",
            "\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"<environment_context>zsh</environment_context>\"}]}}\n",
            "{\"timestamp\":\"2026-07-01T10:00:02Z\",\"type\":\"response_item\",",
            "\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"ship the release\"}]}}",
        ),
        BASE_MS + 5_000,
    );
}

fn cursor_fixture(root: &Path) -> String {
    let workspace = root.join("work/one");
    fs::create_dir_all(&workspace).unwrap();
    let canonical = fs::canonicalize(&workspace).unwrap();
    let encoded: String = canonical
        .to_str()
        .unwrap()
        .trim_start_matches('/')
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    write_at(
        &root.join(format!(
            ".cursor/projects/{encoded}/agent-transcripts/cu-1/cu-1.jsonl"
        )),
        concat!(
            "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"cursor prompt\"}]}}\n",
            "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"cursor answer\"}]}}",
        ),
        BASE_MS + 20_000,
    );
    let db = root.join("Library/Application Support/Cursor/User/globalStorage/state.vscdb");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.pragma_update(None, "journal_mode", "wal").unwrap();
    conn.execute("CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value BLOB)", [])
        .unwrap();
    conn.execute(
        "INSERT INTO cursorDiskKV VALUES ('composerData:cu-1', ?1)",
        [format!(
            r#"{{"composerId":"cu-1","name":"Sidebar chat","createdAt":{},"lastUpdatedAt":{},"context":{{}}}}"#,
            BASE_MS + 10_000,
            BASE_MS + 20_000,
        )],
    )
    .unwrap();
    canonical.to_str().unwrap().to_owned()
}

fn fake_home() -> (TempDir, String) {
    let home = tempfile::tempdir().unwrap();
    claude_fixture(home.path());
    pi_fixture(home.path());
    codex_db_fixture(home.path());
    let cursor_cwd = cursor_fixture(home.path());
    (home, cursor_cwd)
}

#[test]
fn empty_home_lists_nothing() {
    let home = tempfile::tempdir().unwrap();
    let output = run(home.path(), &[]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout_json(&output), Value::Array(Vec::new()));
    assert_eq!(stderr_text(&output), "");
}

#[test]
fn lists_all_agents_sorted_desc_with_full_schema() {
    let (home, cursor_cwd) = fake_home();
    let output = run(home.path(), &[]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stderr_text(&output), "");
    let sessions = stdout_json(&output);
    let ids: Vec<_> = sessions.as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["cx-recent", CLAUDE_A, "cx-seconds", CLAUDE_B, "cu-1", "pi-1"]);
    for session in sessions.as_array().unwrap() {
        let keys: Vec<_> = session.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["agent", "branch", "created_at", "cwd", "id", "path", "title", "updated_at"]);
    }
    assert_eq!(sessions[0]["title"], "Fix tab groups");
    assert_eq!(sessions[1]["title"], "Claude session A");
    assert_eq!(sessions[2]["title"], "explore the API");
    assert_eq!(sessions[2]["branch"], Value::Null);
    assert_eq!(sessions[3]["title"], "hello from B");
    assert_eq!(sessions[3]["branch"], Value::Null);
    assert_eq!(sessions[4]["title"], "Sidebar chat");
    assert_eq!(sessions[4]["cwd"], Value::String(cursor_cwd));
    assert_eq!(sessions[5]["title"], "pi prompt");
    assert_eq!(sessions[0]["created_at"], "2026-05-28T20:26:50Z");
    assert_eq!(sessions[0]["updated_at"], "2026-05-28T20:27:20Z");
    assert_eq!(sessions[2]["updated_at"], "2026-05-28T20:27:10Z");
}

#[test]
fn include_archived_adds_codex_thread() {
    let (home, _) = fake_home();
    let output = run(home.path(), &["--include-archived"]);
    let sessions = stdout_json(&output);
    assert_eq!(sessions[0]["id"], "cx-archived");
    assert_eq!(sessions.as_array().unwrap().len(), 7);
}

#[test]
fn agent_filter_accepts_comma_separated_values() {
    let (home, _) = fake_home();
    let output = run(home.path(), &["--agent", "claude-code,pi"]);
    let agents: Vec<_> = stdout_json(&output)
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["agent"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(agents, ["claude-code", "claude-code", "pi"]);
}

#[test]
fn cwd_filter_matches_path_and_descendants() {
    let (home, _) = fake_home();
    let output = run(home.path(), &["--cwd", "/w/one"]);
    let ids: Vec<_> = stdout_json(&output)
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, ["cx-recent", CLAUDE_A, CLAUDE_B]);
}

#[test]
fn limit_and_ndjson_stream_lines() {
    let (home, _) = fake_home();
    let output = run(home.path(), &["-n", "2", "-f", "ndjson"]);
    let lines: Vec<_> = String::from_utf8_lossy(&output.stdout).trim().lines().map(str::to_owned).collect();
    assert_eq!(lines.len(), 2);
    for line in lines {
        serde_json::from_str::<Value>(&line).unwrap();
    }
}

#[test]
fn table_has_header_and_tab_separated_rows() {
    let (home, _) = fake_home();
    let output = run(home.path(), &["-f", "table", "-n", "1"]);
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some("agent\tupdated_at\tid\tbranch\tcwd\ttitle"));
    assert!(lines.next().unwrap().starts_with("codex\t"));
}

#[test]
fn codex_falls_back_to_rollout_scan_when_db_missing() {
    let home = tempfile::tempdir().unwrap();
    codex_rollout_fixture(home.path());
    let output = run(home.path(), &[]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stderr_text(&output), "");
    let sessions = stdout_json(&output);
    assert_eq!(sessions[0]["id"], "cx-scan");
    assert_eq!(sessions[0]["cwd"], "/w/three");
    assert_eq!(sessions[0]["branch"], "feat");
    assert_eq!(sessions[0]["title"], "ship the release");
    assert_eq!(sessions[0]["created_at"], "2026-07-01T09:59:00Z");
}

#[test]
fn corrupt_codex_db_warns_and_falls_back_to_scan() {
    let home = tempfile::tempdir().unwrap();
    codex_rollout_fixture(home.path());
    write_at(&home.path().join(".codex/state_5.sqlite"), "not a database", BASE_MS);
    let output = run(home.path(), &[]);
    assert_eq!(output.status.code(), Some(0));
    assert!(stderr_text(&output).contains("ap: codex:"));
    assert_eq!(stdout_json(&output)[0]["id"], "cx-scan");
}

#[test]
fn corrupt_cursor_db_fails_only_that_provider() {
    let (home, _) = fake_home();
    let db = home.path().join("Library/Application Support/Cursor/User/globalStorage/state.vscdb");
    fs::write(&db, "not a database").unwrap();
    let output = run(home.path(), &[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr_text(&output).contains("ap: cursor:"));
    let agents: Vec<_> = stdout_json(&output)
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["agent"].as_str().unwrap().to_owned())
        .collect();
    assert!(!agents.contains(&"cursor".to_owned()));
    assert!(agents.contains(&"codex".to_owned()));
    assert!(agents.contains(&"claude-code".to_owned()));
}
