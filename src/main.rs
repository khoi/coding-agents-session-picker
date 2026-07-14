mod cache;
mod conversation;
mod output;
mod pick;
mod providers;
mod scrape;
mod session;

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use rayon::prelude::*;

use crate::output::Format;
use crate::session::{Agent, Session};

#[derive(Parser)]
#[command(
    name = "ap",
    version,
    about = "List local AI coding agent sessions (Claude Code, Codex, Cursor, Pi)"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
    #[arg(short, long, value_enum, default_value = "json")]
    format: Format,
    #[arg(
        short,
        long,
        global = true,
        value_delimiter = ',',
        help = "Only these agents (repeatable or comma-separated)"
    )]
    agent: Vec<Agent>,
    #[arg(long, global = true, value_name = "PATH", help = "Only sessions whose working directory is PATH or inside it")]
    cwd: Option<PathBuf>,
    #[arg(short = 'n', long, global = true, value_name = "N", help = "At most N sessions, applied after sorting")]
    limit: Option<usize>,
    #[arg(long, global = true, help = "Include archived Codex threads")]
    include_archived: bool,
    #[arg(long, global = true, value_name = "DIR", help = "Resolve agent stores under DIR instead of $HOME")]
    root: Option<PathBuf>,
}

#[derive(clap::Subcommand)]
enum Cmd {
    #[command(about = "Pick a session interactively and resume it in its agent")]
    Pick {
        #[arg(long, help = "Start showing all directories instead of the current one")]
        all: bool,
        #[arg(long, value_enum, value_name = "FIELD", help = "Print FIELD to stdout instead of resuming")]
        print: Option<pick::Print>,
    },
}

fn main() -> ExitCode {
    let bare = std::env::args_os().len() == 1;
    let cli = Cli::parse();
    let providers: Vec<_> = providers::all(cli.root.as_deref(), cli.include_archived)
        .into_iter()
        .filter(|provider| cli.agent.is_empty() || cli.agent.contains(&provider.agent()))
        .collect();
    let cache = cache::Cache::load(cli.root.is_none().then(cache::Cache::default_path));
    let results: Vec<_> = providers
        .par_iter()
        .map(|provider| (provider.agent(), provider.sessions(&cache)))
        .collect();
    cache.save();

    let mut failed = false;
    let mut sessions = Vec::new();
    for (agent, result) in results {
        match result {
            Ok(mut found) => sessions.append(&mut found),
            Err(err) => {
                failed = true;
                eprintln!("{}: {agent}: {err:#}", env!("CARGO_BIN_NAME"));
            }
        }
    }
    session::sort_desc(&mut sessions);
    if let Some(limit) = cli.limit {
        sessions.truncate(limit);
    }

    if let Some((all, print)) = picker_options(cli.command.as_ref(), bare) {
        return run_picker(&cli, &sessions, all, print, failed);
    }
    if let Some(base) = &cli.cwd {
        let base = std::fs::canonicalize(base).unwrap_or_else(|_| base.clone());
        sessions.retain(|session| {
            session.cwd.as_ref().is_some_and(|cwd| Path::new(cwd).starts_with(&base))
        });
    }
    match render(cli.format, &sessions) {
        Ok(()) => exit(failed),
        Err(err) if is_broken_pipe(&err) => exit(failed),
        Err(err) => {
            eprintln!("{}: {err:#}", env!("CARGO_BIN_NAME"));
            ExitCode::FAILURE
        }
    }
}

fn picker_options(command: Option<&Cmd>, bare: bool) -> Option<(bool, Option<pick::Print>)> {
    match command {
        Some(Cmd::Pick { all, print }) => Some((*all, *print)),
        None if bare => Some((false, None)),
        None => None,
    }
}

fn run_picker(cli: &Cli, sessions: &[Session], all: bool, print: Option<pick::Print>, failed: bool) -> ExitCode {
    let scope = cli
        .cwd
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .map(|dir| std::fs::canonicalize(&dir).unwrap_or(dir))
        .unwrap_or_default();
    match pick::run(sessions, &scope, !all) {
        Ok(Some(index)) => {
            let session = &sessions[index];
            match print {
                Some(field) => {
                    println!("{}", pick::field(session, field));
                    exit(failed)
                }
                None => {
                    let err = pick::resume(session);
                    eprintln!("{}: {err:#}", env!("CARGO_BIN_NAME"));
                    ExitCode::FAILURE
                }
            }
        }
        Ok(None) => ExitCode::from(130),
        Err(err) => {
            eprintln!("{}: {err:#}", env!("CARGO_BIN_NAME"));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_invocation_defaults_to_picker() {
        assert!(matches!(picker_options(None, true), Some((false, None))));
    }

    #[test]
    fn flag_only_invocation_still_lists() {
        assert!(picker_options(None, false).is_none());
    }
}
