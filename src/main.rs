mod app;
mod clipboard;
mod config;
mod event;
mod host;
mod keys;
mod modes;
mod run;
mod state;
mod ui;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

use crate::{
    config::Config,
    host::HostInfo,
    modes::{
        Mode, Shared,
        files::{FilesMode, fs::Fs, local::LocalFs, memory::MemoryFs},
        terminal::TerminalMode,
    },
    ui::{terminal_guard::TerminalGuard, theme::Theme},
};

/// smtui — server manager TUI: terminals and a file browser for the machine it runs on.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Folder to start in (defaults to the current directory).
    path: Option<PathBuf>,
    /// Config file (defaults to ~/.config/smtui/config.toml).
    #[arg(long)]
    config: Option<PathBuf>,
    /// Use an in-memory sample file system instead of the real one.
    #[arg(long)]
    demo: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = Config::load(cli.config.as_deref())?;
    let _log_guard = init_logging();

    let start_dir = match &cli.path {
        Some(p) => std::fs::canonicalize(p).with_context(|| format!("cannot open {}", p.display()))?,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
    };

    let (tx, rx) = event::channel();
    let shared = Shared {
        theme: Theme::new(&config.accent, config.truecolor),
        // The demo shows a made-up server, matching its sample files.
        host: if cli.demo { HostInfo::demo() } else { HostInfo::detect() },
        config,
        tx: tx.clone(),
        // The demo must not change the user's real preferences.
        state: if cli.demo { state::StateStore::in_memory() } else { state::StateStore::default_location() },
    };

    let (fs, files_dir): (std::sync::Arc<dyn Fs>, PathBuf) = if cli.demo {
        (std::sync::Arc::new(MemoryFs::sample()), PathBuf::from("/home/deploy"))
    } else {
        (std::sync::Arc::new(LocalFs), start_dir.clone())
    };

    let mut guard = TerminalGuard::new().context("failed to set up the terminal")?;
    let modes: Vec<Box<dyn Mode>> = vec![
        Box::new(TerminalMode::new(start_dir, tx.clone())),
        Box::new(FilesMode::new(fs, files_dir, &shared)),
    ];
    let result = app::App::new(shared, modes).run(&mut guard.terminal, rx).await;
    drop(guard);
    result
}

/// Logs go to a file because stdout belongs to the TUI. Level via `SMTUI_LOG` (e.g. `debug`).
fn init_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let dir = run::state_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let appender = tracing_appender::rolling::never(&dir, "smtui.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = tracing_subscriber::EnvFilter::try_from_env("SMTUI_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_writer(writer).with_ansi(false).with_env_filter(filter).init();
    Some(guard)
}
