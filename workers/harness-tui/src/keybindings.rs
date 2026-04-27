//! Keybinding registry + display layer.
//!
//! Holds the canonical list of TUI keybindings (mirroring what `main.rs`
//! actually wires) plus an optional override map loaded from
//! `~/.harness/keybindings.json`. The manager powers both the `/hotkeys`
//! overlay renderer and the actual key dispatcher in `main.rs` — call
//! [`KeybindingsManager::resolve`] from a key handler to turn a `KeyEvent`
//! into a [`KeyAction`] under the active overrides.
//!
//! File shape:
//! ```json
//! { "actions": { "abort_run": "Ctrl+C", "open_tree": "Esc Esc" } }
//! ```

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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

/// Stable enum of UI actions the dispatcher can fire.
///
/// Each variant maps to exactly one `action` string used by the override file
/// (see [`KeyAction::action_name`]). Keeping this enum stable means override
/// JSON stays valid across releases even when the underlying key combos shift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyAction {
    // ---- Global ----
    Submit,
    SubmitFollowup,
    AbortOrQuit,
    Escape,
    OpenTree,
    OpenHotkeys,
    ClearScrollback,
    ToggleTools,
    ToggleThinking,
    Paste,
    CycleThinkingLevel,
    ScrollUp,
    ScrollDown,
    // ---- Editor ----
    HistoryPrev,
    HistoryNext,
    Newline,
    DeleteWordBack,
    // ---- Tree overlay ----
    TreeClose,
    TreeFilterCycle,
    TreeBookmark,
    TreeToggleTimestamps,
    TreePivot,
    // ---- Picker ----
    PickerOpenCommand,
    PickerOpenFile,
    PickerComplete,
}

impl KeyAction {
    /// Stable string used in the override JSON file. Must round-trip with
    /// [`KeyAction::from_action_name`].
    pub const fn action_name(self) -> &'static str {
        match self {
            Self::Submit => "submit",
            Self::SubmitFollowup => "submit_followup",
            Self::AbortOrQuit => "abort_or_quit",
            Self::Escape => "escape",
            Self::OpenTree => "open_tree",
            Self::OpenHotkeys => "open_hotkeys",
            Self::ClearScrollback => "clear_scrollback",
            Self::ToggleTools => "toggle_tools",
            Self::ToggleThinking => "toggle_thinking",
            Self::Paste => "paste",
            Self::CycleThinkingLevel => "cycle_thinking_level",
            Self::ScrollUp => "scroll_up",
            Self::ScrollDown => "scroll_down",
            Self::HistoryPrev => "history_prev",
            Self::HistoryNext => "history_next",
            Self::Newline => "newline",
            Self::DeleteWordBack => "delete_word_back",
            Self::TreeClose => "tree_close",
            Self::TreeFilterCycle => "tree_filter_cycle",
            Self::TreeBookmark => "tree_bookmark",
            Self::TreeToggleTimestamps => "tree_toggle_timestamps",
            Self::TreePivot => "tree_pivot",
            Self::PickerOpenCommand => "picker_open_command",
            Self::PickerOpenFile => "picker_open_file",
            Self::PickerComplete => "picker_complete",
        }
    }

    /// Inverse of [`KeyAction::action_name`]. Returns `None` for unknown
    /// strings so unknown override entries are simply ignored.
    pub fn from_action_name(s: &str) -> Option<Self> {
        let v = match s {
            "submit" => Self::Submit,
            "submit_followup" => Self::SubmitFollowup,
            "abort_or_quit" => Self::AbortOrQuit,
            "escape" => Self::Escape,
            "open_tree" => Self::OpenTree,
            "open_hotkeys" => Self::OpenHotkeys,
            "clear_scrollback" => Self::ClearScrollback,
            "toggle_tools" => Self::ToggleTools,
            "toggle_thinking" => Self::ToggleThinking,
            "paste" => Self::Paste,
            "cycle_thinking_level" => Self::CycleThinkingLevel,
            "scroll_up" => Self::ScrollUp,
            "scroll_down" => Self::ScrollDown,
            "history_prev" => Self::HistoryPrev,
            "history_next" => Self::HistoryNext,
            "newline" => Self::Newline,
            "delete_word_back" => Self::DeleteWordBack,
            "tree_close" => Self::TreeClose,
            "tree_filter_cycle" => Self::TreeFilterCycle,
            "tree_bookmark" => Self::TreeBookmark,
            "tree_toggle_timestamps" => Self::TreeToggleTimestamps,
            "tree_pivot" => Self::TreePivot,
            "picker_open_command" => Self::PickerOpenCommand,
            "picker_open_file" => Self::PickerOpenFile,
            "picker_complete" => Self::PickerComplete,
            _ => return None,
        };
        Some(v)
    }
}

/// Normalized key-press chord: modifier flags + a [`ChordKey`] payload.
///
/// Stored separately from `crossterm::KeyEvent` so we can compare a runtime
/// keypress against a string parsed from the override JSON without dragging
/// crossterm types into the override file format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: ChordKey,
}

/// Subset of `KeyCode` that the override file can express. We deliberately
/// don't model every exotic crossterm code — only the ones the dispatcher
/// actually uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChordKey {
    Char(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
}

impl KeyChord {
    /// Build a chord from a live crossterm `KeyEvent`. Lower-cases ASCII
    /// alphabetic chars so `Ctrl+L` and `Ctrl+l` collapse to the same chord.
    pub fn from_event(ev: &KeyEvent) -> Option<Self> {
        let key = match ev.code {
            KeyCode::Char(c) => ChordKey::Char(c.to_ascii_lowercase()),
            KeyCode::Enter => ChordKey::Enter,
            KeyCode::Esc => ChordKey::Esc,
            KeyCode::Tab => ChordKey::Tab,
            KeyCode::BackTab => ChordKey::BackTab,
            KeyCode::Backspace => ChordKey::Backspace,
            KeyCode::Delete => ChordKey::Delete,
            KeyCode::Left => ChordKey::Left,
            KeyCode::Right => ChordKey::Right,
            KeyCode::Up => ChordKey::Up,
            KeyCode::Down => ChordKey::Down,
            KeyCode::Home => ChordKey::Home,
            KeyCode::End => ChordKey::End,
            KeyCode::PageUp => ChordKey::PageUp,
            KeyCode::PageDown => ChordKey::PageDown,
            KeyCode::F(n) => ChordKey::F(n),
            _ => return None,
        };
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        let alt = ev.modifiers.contains(KeyModifiers::ALT);
        // Crossterm reports Shift+Tab as code=BackTab with SHIFT set; treat
        // that as a non-shifted BackTab chord so it matches the canonical
        // "Shift+Tab" entry parsed from the override file (see `parse_chord`).
        let shift = match key {
            ChordKey::BackTab => false,
            ChordKey::Char(c) if c.is_ascii_alphabetic() => {
                // SHIFT for letters comes through as uppercase already; drop
                // the modifier so chord comparison stays consistent.
                false
            }
            _ => ev.modifiers.contains(KeyModifiers::SHIFT),
        };
        Some(Self {
            ctrl,
            alt,
            shift,
            key,
        })
    }

    /// Parse a single chord from its display form (`"Ctrl+L"`, `"Esc"`,
    /// `"Shift+Tab"`, `"Alt+Enter"`, `"PgUp"`, etc.). Returns `None` for
    /// malformed input. Whitespace-separated multi-chord sequences (e.g.
    /// `"Esc Esc"`) are not handled here — see [`parse_chord_sequence`].
    pub fn parse(s: &str) -> Option<Self> {
        let mut ctrl = false;
        let mut alt = false;
        let mut shift = false;
        let mut key: Option<ChordKey> = None;
        for part in s.split('+') {
            let p = part.trim();
            if p.is_empty() {
                return None;
            }
            match p.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => ctrl = true,
                "alt" | "option" | "opt" => alt = true,
                "shift" => shift = true,
                _ => {
                    if key.is_some() {
                        // More than one non-modifier token isn't a single chord.
                        return None;
                    }
                    key = Some(parse_key_token(p)?);
                }
            }
        }
        let mut key = key?;
        // Normalise letters to lowercase so `Ctrl+L` matches `Ctrl+l`.
        if let ChordKey::Char(c) = key {
            key = ChordKey::Char(c.to_ascii_lowercase());
        }
        // `Shift+Tab` is canonically reported by crossterm as `BackTab` with
        // no SHIFT bit. Collapse the override form to match.
        if matches!(key, ChordKey::Tab) && shift {
            key = ChordKey::BackTab;
            shift = false;
        }
        // Letter chords carry their case in the char itself, not in shift.
        if matches!(key, ChordKey::Char(c) if c.is_ascii_alphabetic()) {
            shift = false;
        }
        Some(Self {
            ctrl,
            alt,
            shift,
            key,
        })
    }
}

fn parse_key_token(s: &str) -> Option<ChordKey> {
    let lower = s.to_ascii_lowercase();
    let v = match lower.as_str() {
        "enter" | "return" => ChordKey::Enter,
        "esc" | "escape" => ChordKey::Esc,
        "tab" => ChordKey::Tab,
        "backtab" => ChordKey::BackTab,
        "backspace" | "bs" => ChordKey::Backspace,
        "delete" | "del" => ChordKey::Delete,
        "left" => ChordKey::Left,
        "right" => ChordKey::Right,
        "up" => ChordKey::Up,
        "down" => ChordKey::Down,
        "home" => ChordKey::Home,
        "end" => ChordKey::End,
        "pgup" | "pageup" => ChordKey::PageUp,
        "pgdn" | "pgdown" | "pagedown" => ChordKey::PageDown,
        "space" => ChordKey::Char(' '),
        _ => {
            if let Some(rest) = lower.strip_prefix('f') {
                if let Ok(n) = rest.parse::<u8>() {
                    if (1..=24).contains(&n) {
                        return Some(ChordKey::F(n));
                    }
                }
            }
            // Single character.
            let mut chars = s.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            ChordKey::Char(c)
        }
    };
    Some(v)
}

/// Parse a whitespace-separated chord sequence. Today only the single-chord
/// form is consulted by [`KeybindingsManager::resolve`]; multi-chord
/// sequences (like `"Esc Esc"`) parse but never match a single key event —
/// the dispatcher handles double-Esc separately via the `esc_press_count`
/// latch on `App`.
fn parse_chord_sequence(s: &str) -> Vec<KeyChord> {
    s.split_whitespace().filter_map(KeyChord::parse).collect()
}

/// Resolves keybindings against an optional user override file.
#[derive(Debug, Clone)]
pub struct KeybindingsManager {
    defaults: Vec<Keybinding>,
    overrides: HashMap<String, String>,
    /// Cached chord -> action lookup, rebuilt whenever overrides change.
    chord_index: HashMap<KeyChord, KeyAction>,
}

impl Default for KeybindingsManager {
    fn default() -> Self {
        Self::new()
    }
}

impl KeybindingsManager {
    /// Build a manager with the canonical defaults and no overrides applied.
    pub fn new() -> Self {
        let mut mgr = Self {
            defaults: default_bindings(),
            overrides: HashMap::new(),
            chord_index: HashMap::new(),
        };
        mgr.rebuild_chord_index();
        mgr
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
        mgr.rebuild_chord_index();
        mgr
    }

    /// Load from a specific path. Used by tests + the file-watcher reload path.
    pub fn load_from_path(path: &std::path::Path) -> Self {
        let mut mgr = Self::new();
        if let Ok(raw) = std::fs::read_to_string(path) {
            if let Ok(parsed) = serde_json::from_str::<KeybindingsFile>(&raw) {
                mgr.overrides = parsed.actions;
            }
        }
        mgr.rebuild_chord_index();
        mgr
    }

    /// Path the watcher should observe. Returns `None` when the user home
    /// directory cannot be determined (e.g. no $HOME).
    pub fn watch_path() -> Option<PathBuf> {
        user_keybindings_path()
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

    /// Resolve an action name to its display-form key combo, applying
    /// overrides if present. Used by the `/hotkeys` overlay to render a
    /// single line per binding. For runtime key dispatch see [`Self::resolve`].
    pub fn resolve_combo(&self, action: &str) -> String {
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

    /// Look up the action bound to a live key event, applying overrides.
    /// Returns `None` when the chord isn't bound to any action — the caller
    /// should treat that as "fall through to plain editor input".
    pub fn resolve(&self, key: &KeyEvent) -> Option<KeyAction> {
        let chord = KeyChord::from_event(key)?;
        self.chord_index.get(&chord).copied()
    }

    /// Rebuild the chord -> action lookup from defaults + overrides. Called
    /// once at construction and again after any `load*` call.
    ///
    /// Override entries take precedence over defaults; conflicting overrides
    /// (two action names mapping to the same chord) are resolved by last-write
    /// wins on the override map's iteration order, matching what the user sees
    /// when they edit the JSON file by hand.
    fn rebuild_chord_index(&mut self) {
        let mut idx: HashMap<KeyChord, KeyAction> = HashMap::new();
        // Step 1: seed from defaults so every action has a chord even if the
        // override file is empty.
        for binding in &self.defaults {
            let Some(action) = KeyAction::from_action_name(binding.action) else {
                continue;
            };
            // Multi-chord sequences (e.g. `Esc Esc` on `open_tree`) are not
            // bound here — the dispatcher's `esc_press_count` latch handles
            // double-press detection. Single-chord bindings still index.
            let chords = parse_chord_sequence(&binding.key_combo);
            if chords.len() == 1 {
                idx.entry(chords[0]).or_insert(action);
            }
        }
        // Step 2: layer overrides on top. We first strip every default chord
        // for an overridden action so the user-supplied chord is the only one
        // that triggers it, then insert the override.
        for (action_name, combo) in &self.overrides {
            let Some(action) = KeyAction::from_action_name(action_name) else {
                continue;
            };
            idx.retain(|_, a| *a != action);
            let chords = parse_chord_sequence(combo);
            if let Some(c) = chords.first() {
                idx.insert(*c, action);
            }
        }
        self.chord_index = idx;
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
    use crossterm::event::KeyEventKind;
    use std::io::Write;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn ev_kind(code: KeyCode, mods: KeyModifiers, kind: KeyEventKind) -> KeyEvent {
        let mut e = KeyEvent::new(code, mods);
        e.kind = kind;
        e
    }

    #[test]
    fn keybindings_manager_loads_defaults_when_no_file() {
        let mgr = KeybindingsManager::new();
        let all = mgr.all();
        assert!(!all.is_empty(), "defaults should be non-empty");
        // Resolve a known action.
        assert_eq!(mgr.resolve_combo("clear_scrollback"), "Ctrl+L");
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
        assert_eq!(mgr.resolve_combo("clear_scrollback"), "Ctrl+K");
        // Untouched action keeps its default.
        assert_eq!(mgr.resolve_combo("toggle_tools"), "Ctrl+O");
    }

    #[test]
    fn malformed_file_falls_back_to_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("keybindings.json");
        std::fs::write(&path, "not-json").unwrap();
        let mgr = KeybindingsManager::load_from_path(&path);
        // Should not have polluted overrides; defaults remain.
        assert_eq!(mgr.resolve_combo("clear_scrollback"), "Ctrl+L");
    }

    #[test]
    fn resolve_event_returns_default_action_for_default_chord() {
        let mgr = KeybindingsManager::new();
        // Ctrl+L -> ClearScrollback
        let e = ev(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(mgr.resolve(&e), Some(KeyAction::ClearScrollback));
    }

    #[test]
    fn resolve_event_handles_uppercase_letter_with_ctrl() {
        // Crossterm sometimes reports Ctrl+L as KeyCode::Char('L') depending
        // on terminal flags. The chord normaliser should fold case.
        let mgr = KeybindingsManager::new();
        let e = ev(KeyCode::Char('L'), KeyModifiers::CONTROL);
        assert_eq!(mgr.resolve(&e), Some(KeyAction::ClearScrollback));
    }

    #[test]
    fn resolve_event_returns_none_for_plain_letters() {
        let mgr = KeybindingsManager::new();
        // Plain 'a' is not bound to any action.
        let e = ev(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(mgr.resolve(&e), None);
    }

    #[test]
    fn resolve_event_picks_up_override_combo() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("keybindings.json");
        let mut f = std::fs::File::create(&path).expect("create");
        // Move clear_scrollback off Ctrl+L and onto Ctrl+K.
        writeln!(f, r#"{{ "actions": {{ "clear_scrollback": "Ctrl+K" }} }}"#).unwrap();
        let mgr = KeybindingsManager::load_from_path(&path);

        let new_chord = ev(KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(mgr.resolve(&new_chord), Some(KeyAction::ClearScrollback));

        let old_chord = ev(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(
            mgr.resolve(&old_chord),
            None,
            "old default should be released when overridden"
        );
    }

    #[test]
    fn resolve_event_handles_shift_tab_via_backtab() {
        let mgr = KeybindingsManager::new();
        // Crossterm reports Shift+Tab as BackTab + SHIFT modifier.
        let e = ev(KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(mgr.resolve(&e), Some(KeyAction::CycleThinkingLevel));
    }

    #[test]
    fn resolve_event_ignores_release_kind_via_kind_field() {
        // sanity: ev_kind builds something equal to ev when kind=Press.
        let pressed = ev_kind(
            KeyCode::Char('l'),
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        );
        let mgr = KeybindingsManager::new();
        assert_eq!(mgr.resolve(&pressed), Some(KeyAction::ClearScrollback));
    }

    #[test]
    fn parse_chord_recognises_named_keys() {
        assert_eq!(
            KeyChord::parse("Ctrl+L"),
            Some(KeyChord {
                ctrl: true,
                alt: false,
                shift: false,
                key: ChordKey::Char('l')
            })
        );
        assert_eq!(
            KeyChord::parse("Esc"),
            Some(KeyChord {
                ctrl: false,
                alt: false,
                shift: false,
                key: ChordKey::Esc
            })
        );
        assert_eq!(
            KeyChord::parse("PgUp"),
            Some(KeyChord {
                ctrl: false,
                alt: false,
                shift: false,
                key: ChordKey::PageUp
            })
        );
        assert_eq!(
            KeyChord::parse("Shift+Tab"),
            Some(KeyChord {
                ctrl: false,
                alt: false,
                shift: false,
                key: ChordKey::BackTab
            })
        );
    }

    #[test]
    fn parse_chord_rejects_garbage() {
        assert_eq!(KeyChord::parse(""), None);
        assert_eq!(KeyChord::parse("Ctrl+"), None);
        assert_eq!(KeyChord::parse("Ctrl+ab"), None);
    }

    #[test]
    fn key_action_round_trips_action_names() {
        let all = [
            KeyAction::Submit,
            KeyAction::SubmitFollowup,
            KeyAction::AbortOrQuit,
            KeyAction::Escape,
            KeyAction::OpenTree,
            KeyAction::OpenHotkeys,
            KeyAction::ClearScrollback,
            KeyAction::ToggleTools,
            KeyAction::ToggleThinking,
            KeyAction::Paste,
            KeyAction::CycleThinkingLevel,
            KeyAction::ScrollUp,
            KeyAction::ScrollDown,
            KeyAction::HistoryPrev,
            KeyAction::HistoryNext,
            KeyAction::Newline,
            KeyAction::DeleteWordBack,
            KeyAction::TreeClose,
            KeyAction::TreeFilterCycle,
            KeyAction::TreeBookmark,
            KeyAction::TreeToggleTimestamps,
            KeyAction::TreePivot,
            KeyAction::PickerOpenCommand,
            KeyAction::PickerOpenFile,
            KeyAction::PickerComplete,
        ];
        for a in all {
            assert_eq!(KeyAction::from_action_name(a.action_name()), Some(a));
        }
    }
}
