mod config;
mod demo;
mod github;
mod model;
mod server;

use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use model::{SCHEMA_VERSION, SessionSnapshot};

/// Local-first GitHub pull-request review.
#[derive(Parser)]
#[command(name = "aside", version, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// GitHub PR URL, e.g. https://github.com/owner/repo/pull/123
    pr_url: Option<String>,

    #[command(flatten)]
    serve: ServeOpts,
}

#[derive(Subcommand)]
enum Command {
    /// Open the UI with bundled mock data (no GitHub access).
    Demo {
        #[command(flatten)]
        serve: ServeOpts,
    },
    /// Set a global config value, e.g. `aside set browser.auto_open false`.
    Set {
        /// Config key in `module.setting` form.
        key: String,
        value: String,
    },
    /// Show one config value, or all values and the config file path.
    Get { key: Option<String> },
}

#[derive(Args, Clone)]
struct ServeOpts {
    /// Do not open the browser (overrides `browser.auto_open`).
    #[arg(long)]
    no_open: bool,
}

fn new_session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}{:x}", nanos, std::process::id())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let (snapshot, opts) = match (cli.command, cli.pr_url) {
        (Some(Command::Set { key, value }), _) => {
            let mut cfg = config::load()?;
            cfg.set(&key, &value)?;
            config::save(&cfg)?;
            eprintln!("{key} = {} ({})", cfg.get(&key)?, config::path()?.display());
            return Ok(());
        }
        (Some(Command::Get { key }), _) => {
            let cfg = config::load()?;
            match key {
                Some(key) => println!("{}", cfg.get(&key)?),
                None => {
                    eprintln!("# {}", config::path()?.display());
                    for (key, help) in config::KEYS {
                        println!("{key} = {}  # {help}", cfg.get(key)?);
                    }
                }
            }
            return Ok(());
        }
        (Some(Command::Demo { serve }), _) => (
            SessionSnapshot {
                schema_version: SCHEMA_VERSION,
                session_id: new_session_id(),
                demo: true,
                pr: demo::pull_request(),
                files: demo::files(),
            },
            serve,
        ),
        (None, Some(url)) => {
            let pr_ref = github::parse_pr_url(&url)?;
            github::check_gh()?;
            eprintln!("Fetching {} …", pr_ref.url());
            let (pr, files) = github::fetch_pr(&pr_ref)?;
            (
                SessionSnapshot {
                    schema_version: SCHEMA_VERSION,
                    session_id: new_session_id(),
                    demo: false,
                    pr,
                    files,
                },
                cli.serve,
            )
        }
        (None, None) => {
            use clap::CommandFactory;
            Cli::command().print_help()?;
            std::process::exit(2);
        }
    };

    let auto_open = config::load()?.browser.auto_open;
    server::serve(Arc::new(snapshot), auto_open && !opts.no_open).await
}
