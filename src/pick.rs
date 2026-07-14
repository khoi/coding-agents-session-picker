use std::fs::{File, OpenOptions};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use anyhow::Context;
use clap::ValueEnum;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use ratatui::Terminal;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, read};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::prelude::CrosstermBackend;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Row, Table, TableState, Wrap};

use crate::conversation::{self, Message};
use crate::session::{Agent, Session};

#[derive(Clone, Copy, ValueEnum)]
pub enum Print {
    Id,
    Path,
    Cwd,
    Json,
}

struct State {
    query: String,
    cursor: usize,
    solo: Option<Agent>,
    scoped: bool,
    selected: usize,
    preview: Option<Result<Vec<Message>, String>>,
}

pub fn run(sessions: &[Session], scope: &Path, scoped: bool) -> anyhow::Result<Option<usize>> {
    let tty = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .context("opening /dev/tty")?;
    enable_raw_mode()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(&tty))?;
    execute!(&tty, EnterAlternateScreen)?;
    let picked = event_loop(&mut terminal, sessions, scope, scoped);
    execute!(&tty, LeaveAlternateScreen)?;
    disable_raw_mode()?;
    picked
}

pub fn field(session: &Session, print: Print) -> String {
    match print {
        Print::Id => session.id.clone(),
        Print::Path => session.path.clone().unwrap_or_default(),
        Print::Cwd => session.cwd.clone().unwrap_or_default(),
        Print::Json => serde_json::to_string(session).unwrap_or_default(),
    }
}

pub fn resume(session: &Session) -> anyhow::Error {
    let mut command = resume_command(session);
    let program = command.get_program().to_string_lossy().into_owned();
    anyhow::Error::new(command.exec()).context(format!("launching {program}"))
}

fn resume_command(session: &Session) -> Command {
    let mut command = match session.agent {
        Agent::ClaudeCode => {
            let mut command = Command::new("claude");
            command.arg("--resume").arg(&session.id);
            command
        }
        Agent::Codex => {
            let mut command = Command::new("codex");
            command.arg("resume").arg(&session.id);
            command
        }
        Agent::Cursor => {
            let mut command = Command::new("cursor-agent");
            command.arg("--resume").arg(&session.id);
            command
        }
        Agent::Pi => {
            let mut command = Command::new("pi");
            command.arg("--session").arg(session.path.as_deref().unwrap_or(&session.id));
            command
        }
    };
    if let Some(cwd) = session.cwd.as_deref().filter(|cwd| Path::new(cwd).is_dir()) {
        command.current_dir(cwd);
    }
    command
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<&File>>,
    sessions: &[Session],
    scope: &Path,
    scoped: bool,
) -> anyhow::Result<Option<usize>> {
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut state = State {
        query: String::new(),
        cursor: 0,
        solo: None,
        scoped,
        selected: 0,
        preview: None,
    };
    let mut preview_dirty = false;
    loop {
        let rows = visible(sessions, scope, &state, &mut matcher);
        state.selected = state.selected.min(rows.len().saturating_sub(1));
        if preview_dirty {
            if state.preview.is_some() {
                state.preview = Some(load_preview(sessions, &rows, state.selected));
            }
            preview_dirty = false;
        }
        terminal.draw(|frame| draw(frame, sessions, scope, &state, &rows))?;
        let Event::Key(key) = read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match action(key) {
            Action::Quit => return Ok(None),
            Action::Accept => {
                if let Some(&index) = rows.get(state.selected) {
                    return Ok(Some(index));
                }
            }
            Action::Move(delta) => {
                let selected = state
                    .selected
                    .saturating_add_signed(delta)
                    .min(rows.len().saturating_sub(1));
                preview_dirty = selected != state.selected;
                state.selected = selected;
            }
            Action::TogglePreview => {
                state.preview = if state.preview.is_some() {
                    None
                } else {
                    Some(load_preview(sessions, &rows, state.selected))
                };
            }
            Action::ToggleScope => {
                state.scoped = !state.scoped;
                preview_dirty = true;
            }
            Action::CycleAgent => {
                state.solo = cycle(state.solo);
                preview_dirty = true;
            }
            Action::SoloAgent(agent) => {
                state.solo = (state.solo != Some(agent)).then_some(agent);
                preview_dirty = true;
            }
            Action::Type(c) => {
                let idx = byte_index(&state.query, state.cursor);
                state.query.insert(idx, c);
                state.cursor += 1;
                state.selected = 0;
                preview_dirty = true;
            }
            Action::Erase => {
                if state.cursor > 0 {
                    let start = byte_index(&state.query, state.cursor - 1);
                    let end = byte_index(&state.query, state.cursor);
                    state.query.replace_range(start..end, "");
                    state.cursor -= 1;
                    state.selected = 0;
                    preview_dirty = true;
                }
            }
            Action::EraseWord => {
                let start = word_start(&state.query, state.cursor);
                if start < state.cursor {
                    let from = byte_index(&state.query, start);
                    let to = byte_index(&state.query, state.cursor);
                    state.query.replace_range(from..to, "");
                    state.cursor = start;
                    state.selected = 0;
                    preview_dirty = true;
                }
            }
            Action::CursorHome => state.cursor = 0,
            Action::CursorEnd => state.cursor = state.query.chars().count(),
            Action::CursorLeft => state.cursor = state.cursor.saturating_sub(1),
            Action::CursorRight => {
                state.cursor = (state.cursor + 1).min(state.query.chars().count())
            }
            Action::None => {}
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Quit,
    Accept,
    Move(isize),
    TogglePreview,
    ToggleScope,
    CycleAgent,
    SoloAgent(Agent),
    Type(char),
    Erase,
    EraseWord,
    CursorHome,
    CursorEnd,
    CursorLeft,
    CursorRight,
    None,
}

fn action(key: KeyEvent) -> Action {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Esc => Action::Quit,
        KeyCode::Char('c') if ctrl => Action::Quit,
        KeyCode::Enter => Action::Accept,
        KeyCode::Up => Action::Move(-1),
        KeyCode::Down => Action::Move(1),
        KeyCode::Char('p') if ctrl => Action::Move(-1),
        KeyCode::Char('n') if ctrl => Action::Move(1),
        KeyCode::PageUp => Action::Move(-10),
        KeyCode::PageDown => Action::Move(10),
        KeyCode::Char(' ') if !ctrl && !alt => Action::TogglePreview,
        KeyCode::Tab => Action::ToggleScope,
        KeyCode::Char('t') if ctrl => Action::CycleAgent,
        KeyCode::Char(c @ '1'..='4') if alt => {
            Action::SoloAgent(Agent::value_variants()[c as usize - '1' as usize])
        }
        KeyCode::Backspace => Action::Erase,
        KeyCode::Home => Action::CursorHome,
        KeyCode::End => Action::CursorEnd,
        KeyCode::Left => Action::CursorLeft,
        KeyCode::Right => Action::CursorRight,
        KeyCode::Char('a') if ctrl => Action::CursorHome,
        KeyCode::Char('e') if ctrl => Action::CursorEnd,
        KeyCode::Char('b') if ctrl => Action::CursorLeft,
        KeyCode::Char('f') if ctrl => Action::CursorRight,
        KeyCode::Char('w') if ctrl => Action::EraseWord,
        KeyCode::Char(c) if !ctrl && !alt => Action::Type(c),
        _ => Action::None,
    }
}

fn byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(byte, _)| byte)
        .unwrap_or(s.len())
}

fn word_start(s: &str, cursor: usize) -> usize {
    let chars: Vec<char> = s.chars().collect();
    let mut i = cursor.min(chars.len());
    while i > 0 && chars[i - 1].is_whitespace() {
        i -= 1;
    }
    while i > 0 && !chars[i - 1].is_whitespace() {
        i -= 1;
    }
    i
}

fn load_preview(
    sessions: &[Session],
    rows: &[usize],
    selected: usize,
) -> Result<Vec<Message>, String> {
    let Some(&index) = rows.get(selected) else {
        return Ok(Vec::new());
    };
    conversation::load(&sessions[index]).map_err(|err| format!("{err:#}"))
}

fn cycle(solo: Option<Agent>) -> Option<Agent> {
    let variants = Agent::value_variants();
    match solo {
        None => Some(variants[0]),
        Some(agent) => variants
            .iter()
            .position(|v| *v == agent)
            .and_then(|i| variants.get(i + 1))
            .copied(),
    }
}

fn visible(sessions: &[Session], scope: &Path, state: &State, matcher: &mut Matcher) -> Vec<usize> {
    let candidates = sessions.iter().enumerate().filter(|(_, session)| {
        state.solo.is_none_or(|solo| session.agent == solo)
            && (!state.scoped || in_scope(session, scope))
    });
    if state.query.is_empty() {
        return candidates.map(|(index, _)| index).collect();
    }
    let pattern = Pattern::parse(&state.query, CaseMatching::Ignore, Normalization::Smart);
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, usize)> = candidates
        .filter_map(|(index, session)| {
            let haystack = format!(
                "{} {} {} {}",
                session.title.as_deref().unwrap_or_default(),
                session.cwd.as_deref().unwrap_or_default(),
                session.branch.as_deref().unwrap_or_default(),
                session.agent,
            );
            pattern
                .score(Utf32Str::new(&haystack, &mut buf), matcher)
                .map(|score| (score, index))
        })
        .collect();
    scored.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, index)| index).collect()
}

fn in_scope(session: &Session, scope: &Path) -> bool {
    session
        .cwd
        .as_ref()
        .is_some_and(|cwd| Path::new(cwd).starts_with(scope))
}

fn draw(
    frame: &mut ratatui::Frame,
    sessions: &[Session],
    scope: &Path,
    state: &State,
    rows: &[usize],
) {
    let [input, status, list, detail, hints] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    frame.render_widget(Line::from(format!("> {}", state.query)), input);
    frame.set_cursor_position((input.x + 2 + state.cursor as u16, input.y));

    let scope_label = if state.scoped {
        format!("cwd ({})", scope.display())
    } else {
        "all directories".to_owned()
    };
    let agent_label = state.solo.map_or("all".to_owned(), |solo| solo.to_string());
    frame.render_widget(
        Line::from(format!("scope: {scope_label} · agent: {agent_label}"))
            .style(Style::new().add_modifier(Modifier::DIM)),
        status,
    );

    let now = jiff::Timestamp::now();
    if let Some(preview) = &state.preview {
        let [table, conversation] =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .areas(list);
        draw_table(frame, sessions, rows, now, table, true, state.selected);
        draw_preview(frame, conversation, preview);
    } else {
        draw_table(frame, sessions, rows, now, list, false, state.selected);
    }

    let selected_cwd = rows
        .get(state.selected)
        .and_then(|&index| sessions[index].cwd.as_deref())
        .unwrap_or_default();
    frame.render_widget(
        Line::from(selected_cwd).style(Style::new().add_modifier(Modifier::DIM)),
        detail,
    );
    frame.render_widget(
        Line::from(format!(
            "{}/{} · ↑↓ move · space preview · enter resume · tab cwd/all · ctrl-a agent · alt-1..4 solo · esc quit",
            rows.len(),
            sessions.len(),
        ))
        .style(Style::new().add_modifier(Modifier::DIM)),
        hints,
    );
}

fn draw_table(
    frame: &mut ratatui::Frame,
    sessions: &[Session],
    rows: &[usize],
    now: jiff::Timestamp,
    area: Rect,
    compact: bool,
    selected: usize,
) {
    let widths = if compact {
        vec![
            Constraint::Length(6),
            Constraint::Length(7),
            Constraint::Min(10),
        ]
    } else {
        vec![
            Constraint::Length(6),
            Constraint::Length(7),
            Constraint::Min(20),
            Constraint::Length(18),
        ]
    };
    let table = Table::new(
        rows.iter().map(|&index| {
            let session = &sessions[index];
            let mut cells = vec![
                table_agent(session.agent),
                relative(now, session.updated_at),
                session.title.clone().unwrap_or_default(),
            ];
            if !compact {
                cells.push(session.branch.clone().unwrap_or_default());
            }
            Row::new(cells)
        }),
        widths,
    )
    .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
    .highlight_symbol("▶ ");
    let mut state = TableState::default().with_selected(Some(selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn table_agent(agent: Agent) -> String {
    match agent {
        Agent::ClaudeCode => "claude".to_owned(),
        agent => agent.to_string(),
    }
}

fn draw_preview(frame: &mut ratatui::Frame, area: Rect, preview: &Result<Vec<Message>, String>) {
    let lines = match preview {
        Ok(messages) if messages.is_empty() => {
            vec![
                Line::from("No conversation available")
                    .style(Style::new().add_modifier(Modifier::DIM)),
            ]
        }
        Ok(messages) => messages
            .iter()
            .map(|message| {
                Line::from(vec![
                    Span::styled(
                        format!("{}: ", message.speaker.label()),
                        Style::new().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(&message.text),
                ])
            })
            .collect(),
        Err(err) => vec![Line::from(err.as_str()).style(Style::new().add_modifier(Modifier::DIM))],
    };
    let paragraph = Paragraph::new(lines)
        .block(Block::bordered().title(" conversation "))
        .wrap(Wrap { trim: false });
    let line_count = paragraph.line_count(area.width.saturating_sub(2));
    let scroll = line_count
        .saturating_sub(usize::from(area.height))
        .min(usize::from(u16::MAX)) as u16;
    frame.render_widget(paragraph.scroll((scroll, 0)), area);
}

fn relative(now: jiff::Timestamp, then: jiff::Timestamp) -> String {
    let seconds = (now.as_second() - then.as_second()).max(0);
    match seconds {
        0..60 => "now".to_owned(),
        60..3_600 => format!("{}m ago", seconds / 60),
        3_600..86_400 => format!("{}h ago", seconds / 3_600),
        86_400..604_800 => format!("{}d ago", seconds / 86_400),
        604_800..31_536_000 => format!("{}w ago", seconds / 604_800),
        _ => format!("{}y ago", seconds / 31_536_000),
    }
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;

    use super::*;

    fn session(agent: Agent, title: &str, cwd: &str) -> Session {
        Session {
            agent,
            id: format!("{agent}-{title}"),
            title: Some(title.to_owned()),
            cwd: Some(cwd.to_owned()),
            branch: None,
            updated_at: "2026-07-01T00:00:00Z".parse().unwrap(),
            path: None,
        }
    }

    fn fixtures() -> Vec<Session> {
        vec![
            session(Agent::Codex, "revamp sidebar", "/w/one"),
            session(Agent::ClaudeCode, "fix login", "/w/one/sub"),
            session(Agent::Pi, "sidebar colors", "/w/two"),
        ]
    }

    fn indices(state: &State, scope: &str) -> Vec<usize> {
        visible(&fixtures(), Path::new(scope), state, &mut Matcher::new(Config::DEFAULT))
    }

    fn state() -> State {
        State {
            query: String::new(),
            cursor: 0,
            solo: None,
            scoped: false,
            selected: 0,
            preview: None,
        }
    }

    #[test]
    fn scope_limits_to_cwd_and_descendants() {
        let mut s = state();
        s.scoped = true;
        assert_eq!(indices(&s, "/w/one"), [0, 1]);
        s.scoped = false;
        assert_eq!(indices(&s, "/w/one"), [0, 1, 2]);
    }

    #[test]
    fn solo_filters_one_agent() {
        let mut s = state();
        s.solo = Some(Agent::Pi);
        assert_eq!(indices(&s, "/"), [2]);
    }

    #[test]
    fn fuzzy_query_ranks_matches() {
        let mut s = state();
        s.query = "sidebar".to_owned();
        assert_eq!(indices(&s, "/"), [0, 2]);
        s.query = "nomatch".to_owned();
        assert!(indices(&s, "/").is_empty());
    }

    #[test]
    fn cycle_walks_all_agents_then_clears() {
        let mut solo = None;
        let mut seen = Vec::new();
        for _ in 0..5 {
            solo = cycle(solo);
            seen.push(solo);
        }
        assert_eq!(
            seen,
            [
                Some(Agent::ClaudeCode),
                Some(Agent::Codex),
                Some(Agent::Cursor),
                Some(Agent::Pi),
                None,
            ]
        );
    }

    #[test]
    fn table_shortens_claude_code() {
        assert_eq!(table_agent(Agent::ClaudeCode), "claude");
        assert_eq!(table_agent(Agent::Codex), "codex");
    }

    #[test]
    fn space_toggles_preview_instead_of_filtering() {
        assert_eq!(
            action(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
            Action::TogglePreview
        );
        assert_eq!(
            action(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            Action::Type('x')
        );
    }

    #[test]
    fn emacs_keys_move_the_cursor() {
        assert_eq!(
            action(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            Action::CursorHome
        );
        assert_eq!(
            action(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL)),
            Action::CursorEnd
        );
        assert_eq!(
            action(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)),
            Action::CursorLeft
        );
        assert_eq!(
            action(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL)),
            Action::CursorRight
        );
        assert_eq!(
            action(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL)),
            Action::CycleAgent
        );
    }

    #[test]
    fn ctrl_w_deletes_preceding_word() {
        assert_eq!(
            action(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL)),
            Action::EraseWord
        );
        assert_eq!(word_start("foo bar", 7), 4);
        assert_eq!(word_start("foo bar  ", 9), 4);
        assert_eq!(word_start("foo", 3), 0);
        assert_eq!(word_start("", 0), 0);
    }

    #[test]
    fn typing_inserts_at_cursor() {
        let mut s = state();
        s.query = "helloworld".to_owned();
        s.cursor = 5;
        let idx = byte_index(&s.query, s.cursor);
        s.query.insert(idx, ' ');
        s.cursor += 1;
        assert_eq!(s.query, "hello world");
        assert_eq!(s.cursor, 6);
    }

    #[test]
    fn preview_renders_conversation_pane() {
        let sessions = fixtures();
        let rows = [0, 1, 2];
        let mut state = state();
        state.preview = Some(Ok(vec![Message {
            speaker: conversation::Speaker::Agent,
            text: "preview answer".into(),
        }]));
        let mut terminal = Terminal::new(TestBackend::new(100, 16)).unwrap();
        terminal
            .draw(|frame| draw(frame, &sessions, Path::new("/"), &state, &rows))
            .unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("conversation"));
        assert!(rendered.contains("agent: preview answer"));

        state.preview = Some(Ok(vec![
            Message {
                speaker: conversation::Speaker::Agent,
                text: "one two three four five six seven eight nine ten".into(),
            },
            Message {
                speaker: conversation::Speaker::Agent,
                text: "latest".into(),
            },
        ]));
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
        terminal
            .draw(|frame| draw(frame, &sessions, Path::new("/"), &state, &rows))
            .unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("agent: latest"));
    }

    #[test]
    fn resume_commands_match_each_agent_cli() {
        let shape = |agent, title: &str| {
            let command = resume_command(&session(agent, title, "/nonexistent"));
            let args: Vec<String> = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            (command.get_program().to_string_lossy().into_owned(), args)
        };
        assert_eq!(
            shape(Agent::ClaudeCode, "a"),
            ("claude".into(), vec!["--resume".into(), "claude-code-a".into()])
        );
        assert_eq!(shape(Agent::Codex, "b"), ("codex".into(), vec!["resume".into(), "codex-b".into()]));
        assert_eq!(
            shape(Agent::Cursor, "c"),
            ("cursor-agent".into(), vec!["--resume".into(), "cursor-c".into()])
        );
        assert_eq!(shape(Agent::Pi, "d"), ("pi".into(), vec!["--session".into(), "pi-d".into()]));
    }

    #[test]
    fn resume_runs_in_session_cwd_when_it_exists() {
        let dir = tempfile::tempdir().unwrap();
        let mut with_dir = session(Agent::Codex, "x", dir.path().to_str().unwrap());
        assert_eq!(resume_command(&with_dir).get_current_dir(), Some(dir.path()));
        with_dir.cwd = Some("/does/not/exist".into());
        assert_eq!(resume_command(&with_dir).get_current_dir(), None);
    }

    #[test]
    fn pi_resume_prefers_transcript_path() {
        let mut pi = session(Agent::Pi, "x", "/w");
        pi.path = Some("/w/sessions/x.jsonl".into());
        let command = resume_command(&pi);
        let args: Vec<_> = command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        assert_eq!(args, ["--session", "/w/sessions/x.jsonl"]);
    }

    #[test]
    fn relative_times_read_naturally() {
        let now: jiff::Timestamp = "2026-07-12T12:00:00Z".parse().unwrap();
        let cases = [
            ("2026-07-12T11:59:30Z", "now"),
            ("2026-07-12T11:15:00Z", "45m ago"),
            ("2026-07-12T09:00:00Z", "3h ago"),
            ("2026-07-10T12:00:00Z", "2d ago"),
            ("2026-06-01T12:00:00Z", "5w ago"),
            ("2024-07-12T12:00:00Z", "2y ago"),
        ];
        for (then, expected) in cases {
            assert_eq!(relative(now, then.parse().unwrap()), expected);
        }
    }
}
