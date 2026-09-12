//! Talking to cargo: workspace metadata and builds that report the produced executable.

use std::io::{self, BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::Deserialize;

/// What `lesto dev` needs to know about the workspace.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub root: PathBuf,
    pub target_dir: PathBuf,
}

#[derive(Deserialize)]
struct Metadata {
    workspace_root: PathBuf,
    target_directory: PathBuf,
}

pub fn metadata() -> io::Result<Workspace> {
    let out = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .stderr(Stdio::inherit())
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other("cargo metadata failed"));
    }
    let meta: Metadata = serde_json::from_slice(&out.stdout)
        .map_err(|e| io::Error::other(format!("cannot parse cargo metadata: {e}")))?;
    Ok(Workspace {
        root: meta.workspace_root,
        target_dir: meta.target_directory,
    })
}

#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    pub package: Option<String>,
    pub release: bool,
}

/// Outcome of `cargo build`.
#[derive(Debug, PartialEq)]
pub enum Built {
    /// The executable of the requested package.
    Executable(PathBuf),
    /// Compiler errors were printed already.
    Failed,
}

/// Run `cargo build`, letting diagnostics through, and find the executable it produced.
pub fn build(opts: &BuildOptions) -> io::Result<Built> {
    let mut cmd = Command::new("cargo");
    cmd.arg("build")
        .arg("--message-format=json-render-diagnostics")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if let Some(p) = &opts.package {
        cmd.args(["-p", p]);
    }
    if opts.release {
        cmd.arg("--release");
    }
    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take().expect("piped");
    let mut lines = Vec::new();
    for line in BufReader::new(stdout).lines() {
        lines.push(line?);
    }
    let status = child.wait()?;
    if !status.success() {
        return Ok(Built::Failed);
    }
    let executables = executables(lines.iter().map(String::as_str), opts.package.as_deref());
    match executables.as_slice() {
        [one] => Ok(Built::Executable(one.clone())),
        [] => Err(io::Error::other(match &opts.package {
            Some(p) => format!("package `{p}` has no binary target"),
            None => "no binary target built; pass `-p <package>`".to_string(),
        })),
        many => Err(io::Error::other(format!(
            "several binaries were built ({}); pass `-p <package>`",
            many.iter()
                .filter_map(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

#[derive(Deserialize)]
struct Artifact<'a> {
    reason: &'a str,
    #[serde(default)]
    package_id: &'a str,
    #[serde(default)]
    executable: Option<PathBuf>,
}

/// Executables among cargo's JSON messages, optionally only those of `package`.
pub fn executables<'a>(
    lines: impl Iterator<Item = &'a str>,
    package: Option<&str>,
) -> Vec<PathBuf> {
    lines
        .filter_map(|line| serde_json::from_str::<Artifact>(line).ok())
        .filter(|a| a.reason == "compiler-artifact")
        .filter(|a| package.is_none_or(|p| package_name(a.package_id) == p))
        .filter_map(|a| a.executable)
        .collect()
}

/// Package name out of a cargo package id.
///
/// Handles `path+file:///dir/name#0.1.0`, `path+file:///dir#name@0.1.0`,
/// `registry+https://...#name@0.1.0` and the old `name 0.1.0 (path+file://...)`.
pub fn package_name(id: &str) -> &str {
    if let Some((url, tail)) = id.split_once('#') {
        return match tail.split_once('@') {
            Some((name, _)) => name,
            None => url.trim_end_matches('/').rsplit('/').next().unwrap_or(url),
        };
    }
    id.split(' ').next().unwrap_or(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_names() {
        assert_eq!(
            package_name("path+file:///w/examples/02-notes#0.1.0"),
            "02-notes"
        );
        assert_eq!(package_name("path+file:///w#notes-db@0.1.0"), "notes-db");
        assert_eq!(
            package_name("registry+https://github.com/rust-lang/crates.io-index#serde@1.0.0"),
            "serde"
        );
        assert_eq!(package_name("notes-db 0.1.0 (path+file:///w)"), "notes-db");
    }

    #[test]
    fn picks_the_requested_executable() {
        let lines = [
            r#"{"reason":"compiler-artifact","package_id":"path+file:///w/a#0.1.0","executable":null}"#,
            r#"{"reason":"compiler-artifact","package_id":"path+file:///w/a#0.1.0","executable":"/t/debug/a"}"#,
            r#"{"reason":"compiler-artifact","package_id":"path+file:///w/b#0.1.0","executable":"/t/debug/b"}"#,
            r#"{"reason":"build-finished","success":true}"#,
            "not json at all",
        ];
        assert_eq!(
            executables(lines.iter().copied(), Some("b")),
            vec![PathBuf::from("/t/debug/b")]
        );
        assert_eq!(executables(lines.iter().copied(), None).len(), 2);
        assert!(executables(lines.iter().copied(), Some("zzz")).is_empty());
    }
}
