use std::fs::{File, OpenOptions};
use std::path::Path;

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
use ratatui::layout::{Constraint, Layout};
use ratatui::prelude::CrosstermBackend;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Row, Table, TableState};

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
    solo: Option<Agent>,
    scoped: bool,
    selected: usize,
}

pub fn run(
    sessions: &[Session],
    scope: &Path,
    scoped: bool,
    print: Print,
) -> anyhow::Result<Option<String>> {
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
    Ok(picked?.map(|index| output(&sessions[index], print)))
}

fn output(session: &Session, print: Print) -> String {
    match print {
        Print::Id => session.id.clone(),
        Print::Path => session.path.clone().unwrap_or_default(),
        Print::Cwd => session.cwd.clone().unwrap_or_default(),
        Print::Json => serde_json::to_string(session).unwrap_or_default(),
    }
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
        solo: None,
        scoped,
        selected: 0,
    };
    loop {
        let rows = visible(sessions, scope, &state, &mut matcher);
        state.selected = state.selected.min(rows.len().saturating_sub(1));
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
            Action::Move(delta) =>

                state.selected = state
                    .selected
                    .saturating_add_signed(delta)
                    .min(rows.len().saturating_sub(1)),
            Action::ToggleScope => state.scoped = !state.scoped,
            Action::CycleAgent => state.solo = cycle(state.solo),
            Action::SoloAgent(agent) => {
                state.solo = (state.solo != Some(agent)).then_some(agent);
            }
            Action::Type(c) => {
                state.query.push(c);
                state.selected = 0;
            }
            Action::Erase => {
                state.query.pop();
                state.selected = 0;
            }
            Action::None => {}
        }
    }
}

enum Action {
    Quit,
    Accept,
    Move(isize),
    ToggleScope,
    CycleAgent,
    SoloAgent(Agent),
    Type(char),
    Erase,
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
        KeyCode::Tab => Action::ToggleScope,
        KeyCode::Char('a') if ctrl => Action::CycleAgent,
        KeyCode::Char(c @ '1'..='4') if alt => {
            Action::SoloAgent(Agent::value_variants()[c as usize - '1' as usize])
        }
        KeyCode::Backspace => Action::Erase,
        KeyCode::Char(c) if !ctrl && !alt => Action::Type(c),
        _ => Action::None,
    }
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
    frame.set_cursor_position((input.x + 2 + state.query.chars().count() as u16, input.y));

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
    let table = Table::new(
        rows.iter().map(|&index| {
            let session = &sessions[index];
            Row::new(vec![
                session.agent.to_string(),
                relative(now, session.updated_at),
                session.title.clone().unwrap_or_default(),
                session.branch.clone().unwrap_or_default(),
            ])
        }),
        [
            Constraint::Length(11),
            Constraint::Length(7),
            Constraint::Min(20),
            Constraint::Length(18),
        ],
    )
    .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
    .highlight_symbol("▶ ");
    let mut table_state = TableState::default().with_selected(Some(state.selected));
    frame.render_stateful_widget(table, list, &mut table_state);

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
            "{}/{} · ↑↓ move · enter select · tab cwd/all · ctrl-a agent · alt-1..4 solo · esc quit",
            rows.len(),
            sessions.len(),
        ))
        .style(Style::new().add_modifier(Modifier::DIM)),
        hints,
    );
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
            solo: None,
            scoped: false,
            selected: 0,
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
