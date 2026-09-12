//! `lesto`: a command-line tool for lesto applications, in the spirit of `fastapi dev`.
//!
//! - `lesto dev`: build, run, and rebuild + restart on every change. The CLI owns the
//!   listening socket and hands it to the application (`LISTEN_FDS`), so requests arriving
//!   during a rebuild wait instead of being refused.
//! - `lesto run`: build and run once, same socket handling.
//! - `lesto new`, `lesto openapi`: reserved, not implemented yet.

mod cargo;
mod process;
mod watch;

use std::net::TcpListener;
use std::process::{Child, ExitCode};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};

use crate::cargo::{BuildOptions, Built};

#[derive(Parser)]
#[command(name = "lesto", version, about = "Run lesto applications", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build, run, and restart on every change (keeps the port open while rebuilding).
    Dev(RunArgs),
    /// Build and run once.
    Run(RunArgs),
    /// Create a new lesto project (not implemented yet).
    New {
        /// Project name.
        name: String,
    },
    /// Print the OpenAPI document of an application (not implemented yet).
    Openapi {
        /// Package to build, as for `cargo build -p`.
        #[arg(short, long)]
        package: Option<String>,
    },
}

#[derive(Args, Clone)]
struct RunArgs {
    /// Package to build, as for `cargo build -p` (default: the current package).
    #[arg(short, long)]
    package: Option<String>,
    /// Address to listen on.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    /// Port to listen on.
    #[arg(long, default_value_t = 8000)]
    port: u16,
    /// Build with `--release`.
    #[arg(long)]
    release: bool,
    /// Arguments passed to the application after `--`.
    #[arg(last = true)]
    args: Vec<String>,
}

const DEBOUNCE: Duration = Duration::from_millis(300);
const GRACE: Duration = Duration::from_secs(2);

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Dev(args) => dev(args),
        Command::Run(args) => run(args),
        Command::New { name } => not_implemented(&format!("lesto new {name}")),
        Command::Openapi { .. } => not_implemented("lesto openapi"),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("lesto: {e}");
            ExitCode::FAILURE
        }
    }
}

fn not_implemented(what: &str) -> std::io::Result<ExitCode> {
    eprintln!("{what}: not implemented yet");
    Ok(ExitCode::from(2))
}

/// The socket the application will serve on. `None` where descriptors cannot be inherited.
fn bind(args: &RunArgs) -> std::io::Result<Option<TcpListener>> {
    if cfg!(unix) {
        TcpListener::bind((args.host.as_str(), args.port)).map(Some)
    } else {
        eprintln!(
            "lesto: socket inheritance is not available on this platform; the application binds the port itself"
        );
        Ok(None)
    }
}

fn build_options(args: &RunArgs) -> BuildOptions {
    BuildOptions {
        package: args.package.clone(),
        release: args.release,
    }
}

fn start(
    args: &RunArgs,
    exe: &std::path::Path,
    listener: Option<&TcpListener>,
) -> std::io::Result<Child> {
    let child = process::spawn(&process::Spawn {
        exe,
        args: &args.args,
        host: &args.host,
        port: args.port,
        listener,
    })?;
    eprintln!(
        "lesto: serving http://{}:{}  (docs at /docs)",
        args.host, args.port
    );
    Ok(child)
}

fn run(args: RunArgs) -> std::io::Result<ExitCode> {
    let listener = bind(&args)?;
    let exe = match cargo::build(&build_options(&args))? {
        Built::Executable(exe) => exe,
        Built::Failed => return Ok(ExitCode::FAILURE),
    };
    let mut child = start(&args, &exe, listener.as_ref())?;
    let status = child.wait()?;
    Ok(match status.code() {
        Some(0) => ExitCode::SUCCESS,
        Some(code) => ExitCode::from(code.clamp(1, 255) as u8),
        None => ExitCode::FAILURE,
    })
}

fn dev(args: RunArgs) -> std::io::Result<ExitCode> {
    let workspace = cargo::metadata()?;
    let listener = bind(&args)?;
    let (_watcher, changes) =
        watch::watch(&workspace.root, &workspace.target_dir).map_err(|e| {
            std::io::Error::other(format!("cannot watch {}: {e}", workspace.root.display()))
        })?;
    eprintln!("lesto: watching {}", workspace.root.display());
    let options = build_options(&args);

    loop {
        let mut child = match cargo::build(&options)? {
            Built::Executable(exe) => Some(start(&args, &exe, listener.as_ref())?),
            Built::Failed => {
                eprintln!("lesto: build failed, waiting for changes...");
                None
            }
        };

        // Wait for a change, noticing if the application exits on its own meanwhile.
        let changed = loop {
            match changes.recv_timeout(Duration::from_millis(200)) {
                Ok(path) => break path,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(std::io::Error::other("file watcher stopped"));
                }
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(c) = child.as_mut() {
                        if let Some(status) = c.try_wait()? {
                            eprintln!(
                                "lesto: application exited ({status}), waiting for changes..."
                            );
                            child = None;
                        }
                    }
                }
            }
        };
        watch::debounce(&changes, DEBOUNCE);
        let shown = changed
            .strip_prefix(&workspace.root)
            .unwrap_or(&changed)
            .display();
        eprintln!("lesto: {shown} changed, rebuilding...");
        if let Some(mut c) = child.take() {
            process::stop(&mut c, GRACE)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dev_arguments() {
        let cli = Cli::parse_from([
            "lesto", "dev", "-p", "notes-db", "--port", "8765", "--", "--flag",
        ]);
        let Command::Dev(args) = cli.command else {
            panic!("expected dev")
        };
        assert_eq!(args.package.as_deref(), Some("notes-db"));
        assert_eq!(args.port, 8765);
        assert_eq!(args.host, "127.0.0.1");
        assert_eq!(args.args, vec!["--flag".to_string()]);
        assert!(!args.release);
    }

    #[test]
    fn parses_reserved_commands() {
        assert!(matches!(
            Cli::parse_from(["lesto", "new", "shop"]).command,
            Command::New { name } if name == "shop"
        ));
        assert!(matches!(
            Cli::parse_from(["lesto", "openapi", "-p", "x"]).command,
            Command::Openapi { package: Some(p) } if p == "x"
        ));
    }
}
