mod output;
mod pick;
mod providers;
mod scrape;
mod session;

use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use rayon::prelude::*;

use crate::output::Format;
use crate::session::{Agent, Session};

#[derive(Parser)]
#[command(version, about = "List local AI coding agent sessions (Claude Code, Codex, Cursor, Pi)")]
struct Cli {
    #[arg(
        short,
        long,
        value_enum,
        help = "Force list output: json | ndjson | table (default: json when piped, picker on a terminal)"
    )]
    format: Option<Format>,
    #[arg(short, long, value_delimiter = ',', help = "Only these agents (repeatable or comma-separated)")]
    agent: Vec<Agent>,
    #[arg(long, value_name = "PATH", help = "Only sessions whose working directory is PATH or inside it")]
    cwd: Option<PathBuf>,
    #[arg(short = 'n', long, value_name = "N", help = "At most N sessions, applied after sorting")]
    limit: Option<usize>,
    #[arg(long, help = "Include archived Codex threads")]
    include_archived: bool,
    #[arg(long, value_name = "DIR", help = "Resolve agent stores under DIR instead of $HOME")]
    root: Option<PathBuf>,
    #[arg(long, help = "Picker: start showing all directories instead of the current one")]
    all: bool,
    #[arg(long, value_enum, default_value = "id", help = "Picker: field printed on selection")]
    print: pick::Print,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let providers: Vec<_> = providers::all(cli.root.as_deref(), cli.include_archived)
        .into_iter()
        .filter(|provider| cli.agent.is_empty() || cli.agent.contains(&provider.agent()))
        .collect();
    let results: Vec<_> = providers
        .par_iter()
        .map(|provider| (provider.agent(), provider.sessions()))
        .collect();

    let mut failed = false;
    let mut sessions = Vec::new();
    for (agent, result) in results {
        match result {
            Ok(mut found) => sessions.append(&mut found),
            Err(err) => {
                failed = true;
                eprintln!("casp: {agent}: {err:#}");
            }
        }
    }
    session::sort_desc(&mut sessions);
    if let Some(limit) = cli.limit {
        sessions.truncate(limit);
    }

    let format = match cli.format {
        Some(format) => format,
        None if io::stdout().is_terminal() => return run_picker(&cli, &sessions, failed),
        None => Format::Json,
    };
    if let Some(base) = &cli.cwd {
        let base = std::fs::canonicalize(base).unwrap_or_else(|_| base.clone());
        sessions.retain(|session| {
            session.cwd.as_ref().is_some_and(|cwd| Path::new(cwd).starts_with(&base))
        });
    }
    match render(format, &sessions) {
        Ok(()) => exit(failed),
        Err(err) if is_broken_pipe(&err) => exit(failed),
        Err(err) => {
            eprintln!("casp: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run_picker(cli: &Cli, sessions: &[Session], failed: bool) -> ExitCode {
    let scope = cli
        .cwd
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .map(|dir| std::fs::canonicalize(&dir).unwrap_or(dir))
        .unwrap_or_default();
    match pick::run(sessions, &scope, !cli.all, cli.print) {
        Ok(Some(selection)) => {
            println!("{selection}");
            exit(failed)
        }
        Ok(None) => ExitCode::from(130),
        Err(err) => {
            eprintln!("casp: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn render(format: Format, sessions: &[Session]) -> anyhow::Result<()> {
    output::render(format, sessions, &mut io::stdout().lock())
}

fn is_broken_pipe(err: &anyhow::Error) -> bool {
    let cause = err.root_cause();
    if let Some(io_err) = cause.downcast_ref::<io::Error>() {
        return io_err.kind() == io::ErrorKind::BrokenPipe;
    }
    if let Some(json_err) = cause.downcast_ref::<serde_json::Error>() {
        return json_err.io_error_kind() == Some(io::ErrorKind::BrokenPipe);
    }
    false
}

fn exit(failed: bool) -> ExitCode {
    if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}
