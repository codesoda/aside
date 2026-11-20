mod config;
mod demo;
mod descriptor;
mod diff;
mod github;
mod model;
mod publish;
mod rules;
mod server;
mod session;
mod skill;
mod store;
mod update;
mod util;

use std::{
    io::{self, IsTerminal, Write},
    net::SocketAddr,
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use serde_json::json;
use tracing_subscriber::EnvFilter;

use crate::{
    github::{GhRunner, PrRef, SystemGh},
    session::Session,
    store::Store,
};

#[derive(Parser)]
#[command(name = "aside", version, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Full GitHub pull-request URL.
    pr_url: Option<String>,
    #[command(flatten)]
    serve: ServeOpts,
    /// Diagnostic log filter, e.g. info or debug (stderr only).
    #[arg(long, global = true, default_value = "warn")]
    log_level: String,
}

#[derive(Subcommand)]
enum Command {
    /// Review a bundled offline capture of cli/cli#14921.
    Demo {
        #[command(flatten)]
        serve: ServeOpts,
    },
    /// Set a global config value.
    Set { key: String, value: String },
    /// Show one config value, or all values.
    Get { key: Option<String> },
    /// Install the embedded Aside skill for coding agents.
    Skill {
        #[command(flatten)]
        agents: SkillAgents,
    },
    /// Inspect existing local sessions.
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Check for a release and optionally install it after confirmation.
    Update {
        /// Check without installing.
        #[arg(long, conflicts_with = "yes")]
        check: bool,
        /// Confirm installation without an interactive prompt.
        #[arg(short = 'y', long)]
        yes: bool,
    },
}

#[derive(Args)]
struct SkillAgents {
    /// Install for Claude Code (~/.claude/skills/aside).
    #[arg(long)]
    claude: bool,
    /// Install for Codex (~/.codex/skills/aside).
    #[arg(long)]
    codex: bool,
    /// Install for agents using ~/.agents/skills/aside.
    #[arg(long)]
    agents: bool,
}

#[derive(Subcommand)]
enum SessionCommand {
    List,
    Status { id: String },
}

#[derive(Args, Clone)]
struct ServeOpts {
    /// Do not open the browser.
    #[arg(long)]
    no_open: bool,
}

fn show_sessions(command: SessionCommand) -> Result<()> {
    match command {
        SessionCommand::List => {
            for (path, descriptor) in descriptor::list()? {
                println!(
                    "{}",
                    json!({
                        "sessionId": descriptor.session_id,
                        "pr": descriptor.pr,
                        "url": descriptor.url,
                        "descriptor": path,
                        "live": descriptor::is_live(&descriptor),
                    })
                );
            }
        }
        SessionCommand::Status { id } => {
            let (path, descriptor, live) = descriptor::status(&id)?;
            println!(
                "{}",
                json!({
                    "sessionId": descriptor.session_id,
                    "pr": descriptor.pr,
                    "url": descriptor.url,
                    "descriptor": path,
                    "live": live,
                })
            );
        }
    }
    Ok(())
}

async fn run_update(check_only: bool, yes: bool) -> Result<()> {
    let status = tokio::task::spawn_blocking(update::check).await??;
    println!(
        "{}",
        json!({
            "event": "update.checked", "current": status.current,
            "latest": status.latest, "available": status.available,
            "target": update::current_target().ok(),
        })
    );
    if !status.available || check_only {
        return Ok(());
    }
    update::current_target()?;
    if !yes {
        if !io::stdin().is_terminal() {
            bail!("an interactive terminal or --yes is required to install an update");
        }
        eprint!("Install Aside v{} in place? [y/N] ", status.latest);
        io::stderr().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            eprintln!("Update cancelled.");
            return Ok(());
        }
    }
    let install_status = status.clone();
    tokio::task::spawn_blocking(move || update::install(&install_status)).await??;
    println!(
        "{}",
        json!({"event":"update.installed", "version":status.latest})
    );
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = terminate.recv() => {},
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

async fn start_session(
    snapshot: model::PrSnapshot,
    pr_ref: Option<PrRef>,
    gh: Option<Arc<dyn GhRunner>>,
    open_browser: bool,
) -> Result<()> {
    let store = if pr_ref.is_some() {
        Store::open(&store::default_path()?)?
    } else {
        Store::open_in_memory()?
    };
    let session = Session::new(snapshot, store, gh, pr_ref)?;
    let ui_token = util::random_hex(32);
    let agent_token = util::random_hex(32);
    let state = server::AppState::new(Arc::clone(&session), ui_token.clone(), agent_token.clone());
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?;
    let url = format!("http://{}/", listener.local_addr()?);
    let pr = session
        .snapshot
        .read()
        .expect("snapshot lock")
        .pr
        .url
        .clone();
    let (path, _guard) = descriptor::write(&session.id, &url, &agent_token, &pr)?;
    println!(
        "{}",
        json!({
            "event": "session.started", "sessionId": session.id,
            "url": url, "apiBase": format!("{url}api/v1"), "descriptor": path, "pr": pr,
        })
    );
    eprintln!("Aside is running at {url} (Ctrl-C to stop)");
    let login = server::login_url(&url, &ui_token);
    if !open_browser {
        eprintln!("Open this private login URL locally: {login}");
    }
    if open_browser && let Err(error) = open::that(login) {
        eprintln!("could not open browser: {error}");
    }
    let mut events = session.events.subscribe();
    let forward = tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => println!("{}", serde_json::to_string(&event).unwrap_or_default()),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    if !session.demo {
        let update_session = Arc::clone(&session);
        tokio::spawn(async move {
            if let Ok(Ok(status)) = tokio::task::spawn_blocking(update::check).await
                && status.available
            {
                update_session.notify(
                    "update.available",
                    json!({
                        "current": status.current, "latest": status.latest,
                        "command": "aside update",
                    }),
                );
            }
        });
    }
    let result = axum::serve(listener, server::router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await;
    publish::wait_for_publication().await;
    forward.abort();
    result.context("Aside server failed")
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_new(&cli.log_level).context("invalid --log-level")?)
        .with_writer(std::io::stderr)
        .init();
    let (snapshot, pr_ref, gh, opts) = match (cli.command, cli.pr_url) {
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
        (Some(Command::Session { command }), _) => return show_sessions(command),
        (Some(Command::Skill { agents }), _) => {
            let home = dirs::home_dir().context("cannot find home directory for agent skills")?;
            let selected = skill::selected(agents.claude, agents.codex, agents.agents);
            for path in skill::install(&home, &selected)? {
                println!("{}", json!({"event": "skill.installed", "path": path}));
            }
            return Ok(());
        }
        (Some(Command::Update { check, yes }), _) => return run_update(check, yes).await,
        (Some(Command::Demo { serve }), _) => (demo::snapshot(0), None, None, serve),
        (None, Some(url)) => {
            let reference = github::parse_pr_url(&url)?;
            if let Some((path, existing)) = descriptor::find_live(&reference.url())? {
                println!(
                    "{}",
                    json!({
                        "event": "session.attached", "sessionId": existing.session_id,
                        "url": existing.url, "apiBase": existing.api_base,
                        "descriptor": path, "pr": existing.pr,
                    })
                );
                if config::load()?.browser.auto_open && !cli.serve.no_open {
                    let _ = open::that(existing.url);
                }
                return Ok(());
            }
            let gh: Arc<dyn GhRunner> = Arc::new(SystemGh);
            github::check_gh(gh.as_ref())?;
            eprintln!("Fetching {} ...", reference.url());
            let snapshot =
                github::fetch_snapshot(gh.as_ref(), &reference, &diff::Limits::default())?;
            (snapshot, Some(reference), Some(gh), cli.serve)
        }
        (None, None) => {
            use clap::CommandFactory;
            Cli::command().print_help()?;
            bail!("provide a PR URL or use `aside demo`");
        }
    };
    let open_browser = config::load()?.browser.auto_open && !opts.no_open;
    start_session(snapshot, pr_ref, gh, open_browser).await
}
