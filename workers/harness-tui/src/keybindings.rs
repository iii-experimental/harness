//! Keybinding registry + display layer.
//!
//! Holds the canonical list of TUI keybindings (mirroring what `main.rs`
//! actually wires) plus an optional override map loaded from
//! `~/.harness/keybindings.json`. For 0.1 the manager is consulted only by the
//! `/hotkeys` overlay renderer; the actual key handler in `main.rs` keeps its
//! hardcoded match arms. Wiring overrides into the dispatcher is tracked as
//! a TODO.
//!
//! File shape:
//! ```json
//! { "actions": { "abort_run": "Ctrl+C", "open_tree": "Esc Esc" } }
//! ```

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One keybinding row, as displayed in the `/hotkeys` overlay.
#[derive(Debug, Clone)]
pub struct Keybinding {
    pub action: &'static str,
    pub key_combo: String,
    pub section: &'static str,
    pub description: &'static str,
}

/// Section labels rendered in the hotkeys overlay. Stored as &'static str so
/// callers can compare with `==` without allocations.
pub const SECTION_GLOBAL: &str = "Global";
pub const SECTION_EDITOR: &str = "Editor";
pub const SECTION_TREE: &str = "Tree";
pub const SECTION_PICKER: &str = "Picker";

/// Persistable file-shape: a single `actions` map of action -> key combo.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KeybindingsFile {
    #[serde(default)]
    pub actions: HashMap<String, String>,
}

/// Resolves keybindings against an optional user override file.
#[derive(Debug, Clone)]
pub struct KeybindingsManager {
    defaults: Vec<Keybinding>,
    overrides: HashMap<String, String>,
}

impl Default for KeybindingsManager {
    fn default() -> Self {
        Self::new()
    }
}

impl KeybindingsManager {
    /// Build a manager with the canonical defaults and no overrides applied.
    pub fn new() -> Self {
        Self {
            defaults: default_bindings(),
            overrides: HashMap::new(),
        }
    }

    /// Build a manager and attempt to merge overrides from
    /// `~/.harness/keybindings.json`. Missing file is not an error; malformed
    /// JSON silently falls back to defaults.
    pub fn load() -> Self {
        let mut mgr = Self::new();
        if let Some(path) = user_keybindings_path() {
            if let Ok(raw) = std::fs::read_to_string(&path) {
                if let Ok(parsed) = serde_json::from_str::<KeybindingsFile>(&raw) {
                    mgr.overrides = parsed.actions;
                }
            }
        }
        mgr
    }

    /// Load from a specific path. Used by tests + future `/reload` plumbing.
    pub fn load_from_path(path: &std::path::Path) -> Self {
        let mut mgr = Self::new();
        if let Ok(raw) = std::fs::read_to_string(path) {
            if let Ok(parsed) = serde_json::from_str::<KeybindingsFile>(&raw) {
                mgr.overrides = parsed.actions;
            }
        }
        mgr
    }

    /// Every binding regardless of section.
    pub fn all(&self) -> Vec<Keybinding> {
        self.defaults
            .iter()
            .map(|b| {
                let combo = self
                    .overrides
                    .get(b.action)
                    .cloned()
                    .unwrap_or_else(|| b.key_combo.clone());
                Keybinding {
                    action: b.action,
                    key_combo: combo,
                    section: b.section,
                    description: b.description,
                }
            })
            .collect()
    }

    /// Filter to one section. Order matches the `default_bindings` order.
    pub fn for_section(&self, section: &str) -> Vec<Keybinding> {
        self.all()
            .into_iter()
            .filter(|b| b.section == section)
            .collect()
    }

    /// Resolve an action name to a key combo, applying overrides if present.
    pub fn resolve(&self, action: &str) -> String {
        if let Some(o) = self.overrides.get(action) {
            return o.clone();
        }
        self.defaults
            .iter()
            .find(|b| b.action == action)
            .map(|b| b.key_combo.clone())
            .unwrap_or_default()
    }

    /// Return any key combos that are duplicated across actions after
    /// applying overrides. Two actions sharing the same combo is the typical
    /// shape of a conflict.
    pub fn detect_conflicts(&self) -> Vec<String> {
        let bindings = self.all();
        let mut seen: HashMap<String, Vec<String>> = HashMap::new();
        for b in &bindings {
            seen.entry(b.key_combo.clone())
                .or_default()
                .push(b.action.to_string());
        }
        let mut out: Vec<String> = Vec::new();
        let mut emitted: HashSet<String> = HashSet::new();
        for (combo, actions) in seen {
            if actions.len() > 1 && emitted.insert(combo.clone()) {
                out.push(combo);
            }
        }
        out.sort();
        out
    }

    /// Section labels, in stable display order.
    pub fn sections(&self) -> &'static [&'static str] {
        &[SECTION_GLOBAL, SECTION_EDITOR, SECTION_TREE, SECTION_PICKER]
    }
}

fn user_keybindings_path() -> Option<PathBuf> {
    let home = directories::UserDirs::new()?;
    Some(home.home_dir().join(".harness").join("keybindings.json"))
}

/// Canonical default keybindings. Reflect what `main.rs` actually wires today.
fn default_bindings() -> Vec<Keybinding> {
    vec![
        Keybinding {
            action: "submit",
            key_combo: "Enter".into(),
            section: SECTION_GLOBAL,
            description: "Submit message (start run, or steer if running)",
        },
        Keybinding {
            action: "submit_followup",
            key_combo: "Alt+Enter".into(),
            section: SECTION_GLOBAL,
            description: "Submit as follow-up (queues after current run)",
        },
        Keybinding {
            action: "abort_or_quit",
            key_combo: "Ctrl+C".into(),
            section: SECTION_GLOBAL,
            description: "Abort running run, or quit if idle",
        },
        Keybinding {
            action: "escape",
            key_combo: "Esc".into(),
            section: SECTION_GLOBAL,
            description: "Close picker / clear editor / drop attachments / abort",
        },
        Keybinding {
            action: "open_tree",
            key_combo: "Esc Esc".into(),
            section: SECTION_GLOBAL,
            description: "Open session tree overlay (or run /tree)",
        },
        Keybinding {
            action: "open_hotkeys",
            key_combo: "Ctrl+H".into(),
            section: SECTION_GLOBAL,
            description: "Open hotkeys overlay (or run /hotkeys)",
        },
        Keybinding {
            action: "clear_scrollback",
            key_combo: "Ctrl+L".into(),
            section: SECTION_GLOBAL,
            description: "Clear scrollback",
        },
        Keybinding {
            action: "toggle_tools",
            key_combo: "Ctrl+O".into(),
            section: SECTION_GLOBAL,
            description: "Toggle collapsed tool output",
        },
        Keybinding {
            action: "toggle_thinking",
            key_combo: "Ctrl+T".into(),
            section: SECTION_GLOBAL,
            description: "Toggle expanded thinking blocks",
        },
        Keybinding {
            action: "paste",
            key_combo: "Ctrl+V".into(),
            section: SECTION_GLOBAL,
            description: "Paste image / text from clipboard",
        },
        Keybinding {
            action: "cycle_thinking_level",
            key_combo: "Shift+Tab".into(),
            section: SECTION_GLOBAL,
            description: "Cycle thinking level (off / minimal / low / medium / high / xhigh)",
        },
        Keybinding {
            action: "scroll_up",
            key_combo: "PgUp".into(),
            section: SECTION_GLOBAL,
            description: "Scroll scrollback up",
        },
        Keybinding {
            action: "scroll_down",
            key_combo: "PgDn".into(),
            section: SECTION_GLOBAL,
            description: "Scroll scrollback down",
        },
        Keybinding {
            action: "history_prev",
            key_combo: "Up".into(),
            section: SECTION_EDITOR,
            description: "Walk back through submitted history (single-line) / row up",
        },
        Keybinding {
            action: "history_next",
            key_combo: "Down".into(),
            section: SECTION_EDITOR,
            description: "Walk forward through history / row down",
        },
        Keybinding {
            action: "newline",
            key_combo: "Shift+Enter".into(),
            section: SECTION_EDITOR,
            description: "Insert newline in editor",
        },
        Keybinding {
            action: "delete_word_back",
            key_combo: "Ctrl+W".into(),
            section: SECTION_EDITOR,
            description: "Delete previous word",
        },
        Keybinding {
            action: "tree_close",
            key_combo: "Esc".into(),
            section: SECTION_TREE,
            description: "Close session tree overlay",
        },
        Keybinding {
            action: "tree_filter_cycle",
            key_combo: "Ctrl+O".into(),
            section: SECTION_TREE,
            description: "Cycle filter: Default / NoTools / UserOnly / Labeled / All",
        },
        Keybinding {
            action: "tree_bookmark",
            key_combo: "Shift+L".into(),
            section: SECTION_TREE,
            description: "Toggle bookmark on focused message",
        },
        Keybinding {
            action: "tree_toggle_timestamps",
            key_combo: "Shift+T".into(),
            section: SECTION_TREE,
            description: "Toggle timestamp display in tree",
        },
        Keybinding {
            action: "tree_pivot",
            key_combo: "Enter".into(),
            section: SECTION_TREE,
            description: "Pivot focused message to top (visual highlight only)",
        },
        Keybinding {
            action: "picker_open_command",
            key_combo: "/".into(),
            section: SECTION_PICKER,
            description: "Open slash command picker",
        },
        Keybinding {
            action: "picker_open_file",
            key_combo: "@".into(),
            section: SECTION_PICKER,
            description: "Open @file fuzzy picker",
        },
        Keybinding {
            action: "picker_complete",
            key_combo: "Tab".into(),
            section: SECTION_PICKER,
            description: "Complete highlighted picker entry",
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn keybindings_manager_loads_defaults_when_no_file() {
        let mgr = KeybindingsManager::new();
        let all = mgr.all();
        assert!(!all.is_empty(), "defaults should be non-empty");
        // Resolve a known action.
        assert_eq!(mgr.resolve("clear_scrollback"), "Ctrl+L");
    }

    #[test]
    fn detect_conflicts_finds_duplicate_combos() {
        let mgr = KeybindingsManager::new();
        let conflicts = mgr.detect_conflicts();
        // Defaults intentionally use Ctrl+O for both global toggle_tools and
        // tree filter cycle (overlay context). Same with Esc for global +
        // tree close. detect_conflicts should surface those.
        assert!(
            conflicts.iter().any(|c| c == "Ctrl+O"),
            "Ctrl+O should be flagged: {conflicts:?}"
        );
    }

    #[test]
    fn for_section_filters_to_section_only() {
        let mgr = KeybindingsManager::new();
        let global = mgr.for_section(SECTION_GLOBAL);
        assert!(global.iter().all(|b| b.section == SECTION_GLOBAL));
        let tree = mgr.for_section(SECTION_TREE);
        assert!(tree.iter().all(|b| b.section == SECTION_TREE));
        assert!(tree.iter().any(|b| b.action == "tree_filter_cycle"));
    }

    #[test]
    fn resolve_returns_override_when_present() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("keybindings.json");
        let mut f = std::fs::File::create(&path).expect("create");
        writeln!(f, r#"{{ "actions": {{ "clear_scrollback": "Ctrl+K" }} }}"#).unwrap();
        let mgr = KeybindingsManager::load_from_path(&path);
        assert_eq!(mgr.resolve("clear_scrollback"), "Ctrl+K");
        // Untouched action keeps its default.
        assert_eq!(mgr.resolve("toggle_tools"), "Ctrl+O");
    }

    #[test]
    fn malformed_file_falls_back_to_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("keybindings.json");
        std::fs::write(&path, "not-json").unwrap();
        let mgr = KeybindingsManager::load_from_path(&path);
        // Should not have polluted overrides; defaults remain.
        assert_eq!(mgr.resolve("clear_scrollback"), "Ctrl+L");
    }
}
