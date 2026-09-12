//! Watching the workspace for changes worth a rebuild.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use notify::{RecursiveMode, Watcher};

/// A file changed at this path.
pub type Change = PathBuf;

/// Watch `root` recursively; deliver one message per relevant changed file.
pub fn watch(
    root: &Path,
    target_dir: &Path,
) -> notify::Result<(impl Watcher + use<>, Receiver<Change>)> {
    let (tx, rx) = mpsc::channel::<Change>();
    let root_owned = root.to_path_buf();
    let target = target_dir.to_path_buf();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event {
            for path in event.paths {
                if is_relevant(&path, &root_owned, &target) {
                    let _ = tx.send(path);
                }
            }
        }
    })?;
    watcher.watch(root, RecursiveMode::Recursive)?;
    Ok((watcher, rx))
}

/// Skip build output, VCS metadata, hidden files and editor temporaries.
pub fn is_relevant(path: &Path, root: &Path, target_dir: &Path) -> bool {
    if path.starts_with(target_dir) {
        return false;
    }
    let relative = path.strip_prefix(root).unwrap_or(path);
    for component in relative.components() {
        let name = component.as_os_str().to_string_lossy();
        if name.starts_with('.') || name == "target" {
            return false;
        }
    }
    let file = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    !(file.ends_with('~')
        || file.ends_with(".swp")
        || file.ends_with(".swx")
        || file.starts_with('#')
        || file.ends_with(".tmp"))
}

/// After a first change, keep collecting for `quiet` so one save does not trigger two builds.
pub fn debounce(rx: &Receiver<Change>, quiet: Duration) {
    while rx.recv_timeout(quiet).is_ok() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters() {
        let root = Path::new("/w");
        let target = Path::new("/w/target");
        let ok = |p: &str| is_relevant(Path::new(p), root, target);
        assert!(ok("/w/crates/lesto/src/lib.rs"));
        assert!(ok("/w/Cargo.toml"));
        assert!(!ok("/w/target/debug/lesto"));
        assert!(!ok("/w/.git/index"));
        assert!(!ok("/w/src/.lib.rs.swp"));
        assert!(!ok("/w/src/lib.rs~"));
        assert!(!ok("/w/src/#lib.rs#"));
        assert!(!ok("/w/examples/01-hello/target/x"));
    }
}
