//! `lesto`: a command-line tool for lesto applications, in the spirit of `fastapi dev`.
//!
//! - `lesto dev`: build, run, and rebuild + restart on every change. The CLI owns the
//!   listening socket and hands it to the application (`LISTEN_FDS`), so requests arriving
//!   during a rebuild wait instead of being refused.
//! - `lesto run`: build and run once, same socket handling.
//! - `lesto openapi`: build the application and print its OpenAPI document, without serving.

mod cargo;
mod process;
mod watch;

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, ExitCode, Stdio};
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
    /// Print the OpenAPI document of an application, without serving it.
    ///
    /// Builds and starts the application with `LESTO_OPENAPI_PATH` set: `App::serve` writes the
    /// document there and returns instead of listening. Whatever `main` does before calling
    /// `App::serve` (a database connection, migrations) still runs.
    Openapi {
        /// Package to build, as for `cargo build -p`.
        #[arg(short, long)]
        package: Option<String>,
        /// Write the document to this file instead of standard output.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Arguments passed to the application after `--`.
        #[arg(last = true)]
        args: Vec<String>,
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
        Command::Openapi {
            package,
            output,
            args,
        } => openapi(package, output, &args),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("lesto: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Read by `App::serve` in the application (`lesto::app`): write the document here, do not
/// serve.
const OPENAPI_PATH_VAR: &str = "LESTO_OPENAPI_PATH";
/// How long `lesto openapi` waits for the application to reach `App::serve`.
const OPENAPI_TIMEOUT: Duration = Duration::from_secs(60);

fn openapi(
    package: Option<String>,
    output: Option<PathBuf>,
    args: &[String],
) -> std::io::Result<ExitCode> {
    let workspace = cargo::metadata()?;
    let options = BuildOptions {
        package,
        release: false,
    };
    let exe = match cargo::build(&options)? {
        Built::Executable(exe) => exe,
        Built::Failed => return Ok(ExitCode::FAILURE),
    };
    let path = workspace
        .target_dir
        .join(format!("lesto-openapi-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&path);
    // The application's own output goes to stderr: stdout carries only the document.
    let mut child = std::process::Command::new(&exe)
        .args(args)
        .env(OPENAPI_PATH_VAR, &path)
        .env_remove("LISTEN_FDS")
        .env_remove("LISTEN_PID")
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::io::stderr()))
        .spawn()?;
    let deadline = std::time::Instant::now() + OPENAPI_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            process::stop(&mut child, GRACE)?;
            let _ = std::fs::remove_file(&path);
            return Err(std::io::Error::other(format!(
                "the application did not reach `App::serve` within {} s; \
                 `lesto openapi` needs `main` to call `App::serve` (or `lambda::serve`)",
                OPENAPI_TIMEOUT.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let document = std::fs::read(&path);
    let _ = std::fs::remove_file(&path);
    let document = match document {
        Ok(document) if status.success() => document,
        Ok(_) => {
            return Err(std::io::Error::other(format!(
                "the application exited with {status}"
            )));
        }
        Err(_) => {
            return Err(std::io::Error::other(format!(
                "the application exited ({status}) without writing its OpenAPI document; \
                 `lesto openapi` needs `main` to call `App::serve` (or `lambda::serve`)"
            )));
        }
    };
    match output {
        Some(output) => std::fs::write(output, document)?,
        None => {
            use std::io::Write;
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(&document)?;
            stdout.write_all(b"\n")?;
        }
    }
    Ok(ExitCode::SUCCESS)
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
                    if let Some(c) = child.as_mut()
                        && let Some(status) = c.try_wait()?
                    {
                        eprintln!("lesto: application exited ({status}), waiting for changes...");
                        child = None;
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
    fn parses_openapi_arguments() {
        assert!(matches!(
            Cli::parse_from(["lesto", "openapi", "-p", "x", "-o", "api.json"]).command,
            Command::Openapi { package: Some(p), output: Some(o), .. }
                if p == "x" && o == std::path::Path::new("api.json")
        ));
    }

    #[test]
    fn new_is_not_a_command_yet() {
        assert!(Cli::try_parse_from(["lesto", "new", "shop"]).is_err());
    }
}
