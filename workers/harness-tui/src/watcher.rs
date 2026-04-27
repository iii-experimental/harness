//! Filesystem watcher for the two user-editable TUI config files.
//!
//! Watches `~/.harness/keybindings.json` and `~/.harness/themes/<active>.toml`
//! and forwards a [`ConfigReloadEvent`] over an mpsc channel whenever either
//! changes on disk. The main event loop selects on the channel alongside
//! crossterm input and re-loads the affected manager in-place.
//!
//! Implementation notes:
//! - We watch the *parent directory* rather than each file. The user often
//!   edits `keybindings.json` by saving via a temp file + rename, which
//!   wouldn't fire events on the original inode. A directory watch picks up
//!   create/rename/modify all at once.
//! - The native watcher runs on its own OS thread (spawned by the `notify`
//!   crate). The shim thread we own here filters events to the two paths we
//!   care about and forwards a tagged enum over the bounded mpsc.
//! - A short debounce (250 ms) collapses bursts of write events that happen
//!   when an editor saves a file (truncate -> write -> close on macOS).

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use notify::event::{EventKind, ModifyKind};
use notify::{recommended_watcher, Event, RecommendedWatcher, RecursiveMode, Watcher};

/// Tagged change emitted by [`ConfigWatcher`] whenever one of the watched
/// files is created, modified, or renamed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigReloadEvent {
    Keybindings,
    Theme,
}

/// Owns the running watcher thread + the receiver end of the reload channel.
///
/// Drop the value to stop watching: the underlying `notify::Watcher` is
/// terminated and the dispatcher thread exits when its sender hangs up.
pub struct ConfigWatcher {
    rx: Receiver<ConfigReloadEvent>,
    // Held to keep the OS-side watcher alive for the lifetime of the struct.
    _watcher: RecommendedWatcher,
}

impl ConfigWatcher {
    /// Spawn the watcher. `keybindings_path` is the file path the user edits
    /// (typically `~/.harness/keybindings.json`); `theme_path` is the
    /// currently active theme file (`~/.harness/themes/<name>.toml`) or any
    /// other path the renderer wants to react to. Either path may be missing
    /// at startup — the watcher attaches to the parent directory instead.
    ///
    /// Returns `None` when no parent directory could be determined for either
    /// path; callers should treat this as "hot-reload not available" and
    /// continue without it.
    pub fn spawn(keybindings_path: PathBuf, theme_path: Option<PathBuf>) -> Option<Self> {
        // Pick the parent directories to watch. We dedupe in case both files
        // live under the same `~/.harness` dir (the common case).
        let mut dirs: Vec<PathBuf> = Vec::new();
        if let Some(parent) = keybindings_path.parent() {
            dirs.push(parent.to_path_buf());
        }
        if let Some(theme) = &theme_path {
            if let Some(parent) = theme.parent() {
                let p = parent.to_path_buf();
                if !dirs.contains(&p) {
                    dirs.push(p);
                }
            }
        }
        if dirs.is_empty() {
            return None;
        }

        let (raw_tx, raw_rx) = mpsc::channel::<notify::Result<Event>>();
        let mut watcher: RecommendedWatcher = recommended_watcher(move |res| {
            // Best-effort send: the receiver may have been dropped on shutdown.
            let _ = raw_tx.send(res);
        })
        .ok()?;

        for dir in &dirs {
            // Ensure the dir exists so notify doesn't bail on attach. Missing
            // is fine — the user may not have created `~/.harness` yet.
            if !dir.exists() {
                let _ = std::fs::create_dir_all(dir);
            }
            // Attach non-recursively; we only care about direct children.
            let _ = watcher.watch(dir, RecursiveMode::NonRecursive);
        }

        let (tx, rx) = mpsc::channel::<ConfigReloadEvent>();
        std::thread::spawn(move || {
            dispatch_loop(raw_rx, tx, keybindings_path, theme_path);
        });

        Some(Self {
            rx,
            _watcher: watcher,
        })
    }

    /// Non-blocking poll for a pending reload event.
    pub fn try_recv(&self) -> Option<ConfigReloadEvent> {
        self.rx.try_recv().ok()
    }
}

/// Translate raw `notify` events into `ConfigReloadEvent`s, debouncing
/// duplicates that arrive within `DEBOUNCE` of the previous emission.
fn dispatch_loop(
    raw_rx: Receiver<notify::Result<Event>>,
    tx: Sender<ConfigReloadEvent>,
    keybindings_path: PathBuf,
    theme_path: Option<PathBuf>,
) {
    const DEBOUNCE: Duration = Duration::from_millis(250);
    let mut last_keybindings: Option<Instant> = None;
    let mut last_theme: Option<Instant> = None;

    while let Ok(res) = raw_rx.recv() {
        let Ok(event) = res else { continue };
        if !is_relevant_kind(event.kind) {
            continue;
        }
        for path in &event.paths {
            if path_matches(path, &keybindings_path)
                && should_emit(&mut last_keybindings, DEBOUNCE)
                && tx.send(ConfigReloadEvent::Keybindings).is_err()
            {
                return;
            }
            if let Some(theme) = &theme_path {
                if path_matches(path, theme)
                    && should_emit(&mut last_theme, DEBOUNCE)
                    && tx.send(ConfigReloadEvent::Theme).is_err()
                {
                    return;
                }
            }
        }
    }
}

fn is_relevant_kind(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_)
            | EventKind::Modify(
                ModifyKind::Data(_) | ModifyKind::Name(_) | ModifyKind::Any | ModifyKind::Other,
            )
    )
}

/// Compare two paths, falling back to lexical equality when canonicalisation
/// fails (e.g. the file was deleted). Avoids a false negative when the user's
/// editor swaps the file inode on save.
fn path_matches(observed: &Path, target: &Path) -> bool {
    if observed == target {
        return true;
    }
    if let (Ok(a), Ok(b)) = (observed.canonicalize(), target.canonicalize()) {
        return a == b;
    }
    // Fall back to filename + parent comparison so post-save rename hits.
    match (observed.file_name(), target.file_name()) {
        (Some(a), Some(b)) if a == b => match (observed.parent(), target.parent()) {
            (Some(pa), Some(pb)) => pa == pb,
            _ => false,
        },
        _ => false,
    }
}

fn should_emit(last: &mut Option<Instant>, debounce: Duration) -> bool {
    let now = Instant::now();
    let ok = match *last {
        Some(t) => now.duration_since(t) >= debounce,
        None => true,
    };
    if ok {
        *last = Some(now);
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn debounce_drops_rapid_repeats() {
        let mut last: Option<Instant> = None;
        assert!(should_emit(&mut last, Duration::from_millis(100)));
        assert!(!should_emit(&mut last, Duration::from_millis(100)));
    }

    #[test]
    fn debounce_allows_after_window() {
        let mut last: Option<Instant> = None;
        assert!(should_emit(&mut last, Duration::from_millis(1)));
        std::thread::sleep(Duration::from_millis(5));
        assert!(should_emit(&mut last, Duration::from_millis(1)));
    }

    #[test]
    fn path_matches_canonical_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("a.json");
        std::fs::write(&p, "{}").unwrap();
        assert!(path_matches(&p, &p));
    }

    #[test]
    fn path_matches_filename_when_canonicalize_fails() {
        // Two non-existent paths under the same parent with the same name.
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("a.json");
        let b = dir.path().join("a.json");
        assert!(path_matches(&a, &b));
    }

    #[test]
    fn path_matches_rejects_unrelated_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("a.json");
        let b = dir.path().join("b.json");
        assert!(!path_matches(&a, &b));
    }

    #[test]
    fn watcher_emits_on_keybinding_write() {
        let dir = tempfile::tempdir().expect("tempdir");
        let kb = dir.path().join("keybindings.json");
        let theme = dir.path().join("themes").join("dark.toml");
        std::fs::create_dir_all(theme.parent().unwrap()).unwrap();

        let watcher = ConfigWatcher::spawn(kb.clone(), Some(theme)).expect("watcher spawned");

        // Give notify a moment to register the watch.
        std::thread::sleep(Duration::from_millis(100));
        std::fs::write(&kb, r#"{"actions":{"clear_scrollback":"Ctrl+K"}}"#).unwrap();

        // Poll up to ~2s for the event to land.
        let mut got = None;
        for _ in 0..40 {
            if let Some(ev) = watcher.try_recv() {
                got = Some(ev);
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(got, Some(ConfigReloadEvent::Keybindings));
    }
}
