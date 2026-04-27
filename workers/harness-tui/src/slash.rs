//! Slash-command registry.
//!
//! Pure data + matching logic; the actual handlers live in [`crate::app`] and
//! [`crate::main`] because they need access to `App` mutation, the runtime
//! handle, or process-level side effects.

/// One entry shown in the autocomplete picker. Names are stored without the
/// leading slash; `description` is rendered in the picker.
#[derive(Debug, Clone, Copy)]
pub struct SlashEntry {
    pub name: &'static str,
    pub description: &'static str,
    /// Whether the command is wired up. Unimplemented commands still appear in
    /// the picker but route to a "not yet implemented" notification.
    pub implemented: bool,
}

/// Static registry of every slash command the TUI knows about.
#[derive(Debug, Clone)]
pub struct SlashCommandRegistry {
    entries: Vec<SlashEntry>,
}

impl Default for SlashCommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SlashCommandRegistry {
    pub fn new() -> Self {
        Self {
            entries: vec![
                SlashEntry {
                    name: "help",
                    description: "Show hotkeys overlay",
                    implemented: true,
                },
                SlashEntry {
                    name: "model",
                    description: "Switch active model: /model <id>",
                    implemented: true,
                },
                SlashEntry {
                    name: "new",
                    description: "Start a fresh session",
                    implemented: true,
                },
                SlashEntry {
                    name: "name",
                    description: "Set session display name: /name <name>",
                    implemented: true,
                },
                SlashEntry {
                    name: "session",
                    description: "Print session id, message + token totals",
                    implemented: true,
                },
                SlashEntry {
                    name: "copy",
                    description: "Copy last assistant message to clipboard",
                    implemented: true,
                },
                SlashEntry {
                    name: "clear",
                    description: "Clear scrollback (alias Ctrl+L)",
                    implemented: true,
                },
                SlashEntry {
                    name: "quit",
                    description: "Exit harness-tui",
                    implemented: true,
                },
                SlashEntry {
                    name: "cwd",
                    description: "Change working directory: /cwd <path>",
                    implemented: true,
                },
                SlashEntry {
                    name: "abort",
                    description: "Abort the running loop",
                    implemented: true,
                },
                SlashEntry {
                    name: "hotkeys",
                    description: "Print hotkey list to scrollback",
                    implemented: true,
                },
                SlashEntry {
                    name: "tree",
                    description: "Show session tree overlay",
                    implemented: true,
                },
                SlashEntry {
                    name: "resume",
                    description: "Resume a previous session",
                    implemented: false,
                },
                SlashEntry {
                    name: "fork",
                    description: "Fork the current session",
                    implemented: false,
                },
                SlashEntry {
                    name: "clone",
                    description: "Clone the current session",
                    implemented: false,
                },
                SlashEntry {
                    name: "compact",
                    description: "Compact context window",
                    implemented: false,
                },
                SlashEntry {
                    name: "export",
                    description: "Export session transcript",
                    implemented: false,
                },
                SlashEntry {
                    name: "share",
                    description: "Share session link",
                    implemented: false,
                },
                SlashEntry {
                    name: "reload",
                    description: "Reload theme + keybindings from ~/.harness",
                    implemented: true,
                },
            ],
        }
    }

    pub fn entries(&self) -> &[SlashEntry] {
        &self.entries
    }

    /// Return every entry whose name starts with `prefix` (without slash).
    pub fn match_prefix(&self, prefix: &str) -> Vec<&SlashEntry> {
        let needle = prefix.strip_prefix('/').unwrap_or(prefix);
        self.entries
            .iter()
            .filter(|e| e.name.starts_with(needle))
            .collect()
    }

    /// Longest unambiguous completion for `prefix` (returns name without
    /// slash). `None` when there are no matches.
    pub fn complete(&self, prefix: &str) -> Option<String> {
        let matches = self.match_prefix(prefix);
        if matches.is_empty() {
            return None;
        }
        if matches.len() == 1 {
            return Some(matches[0].name.to_string());
        }
        // Reduce to common prefix across all match names.
        let mut common: String = matches[0].name.to_string();
        for m in matches.iter().skip(1) {
            let mut shared = 0usize;
            for (a, b) in common.bytes().zip(m.name.bytes()) {
                if a != b {
                    break;
                }
                shared += 1;
            }
            common.truncate(shared);
            if common.is_empty() {
                return None;
            }
        }
        Some(common)
    }

    pub fn get(&self, name: &str) -> Option<&SlashEntry> {
        let needle = name.strip_prefix('/').unwrap_or(name);
        self.entries.iter().find(|e| e.name == needle)
    }
}

/// Parsed slash invocation: command name + remaining argument string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSlash {
    pub name: String,
    pub args: String,
}

/// Parse `"/cmd rest of line"` into `(name, args)`. Returns `None` when the
/// input doesn't begin with `/`.
pub fn parse_slash(input: &str) -> Option<ParsedSlash> {
    let rest = input.trim_start().strip_prefix('/')?;
    let mut split = rest.splitn(2, char::is_whitespace);
    let name = split.next().unwrap_or("").to_string();
    let args = split.next().unwrap_or("").trim().to_string();
    if name.is_empty() {
        return None;
    }
    Some(ParsedSlash { name, args })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_prefix_returns_starts_with_set() {
        let r = SlashCommandRegistry::new();
        let names: Vec<_> = r.match_prefix("/c").iter().map(|e| e.name).collect();
        assert!(names.contains(&"clear"));
        assert!(names.contains(&"copy"));
        assert!(names.contains(&"cwd"));
        assert!(names.contains(&"clone"));
        assert!(names.contains(&"compact"));
    }

    #[test]
    fn complete_to_shared_prefix() {
        let r = SlashCommandRegistry::new();
        // "cl" matches "clear" + "clone". Shared prefix is "cl".
        assert_eq!(r.complete("/cl").as_deref(), Some("cl"));
    }

    #[test]
    fn complete_unique_returns_full_name() {
        let r = SlashCommandRegistry::new();
        assert_eq!(r.complete("/he").as_deref(), Some("help"));
    }

    #[test]
    fn complete_no_match_is_none() {
        let r = SlashCommandRegistry::new();
        assert!(r.complete("/zzz").is_none());
    }

    #[test]
    fn parse_slash_splits_name_and_args() {
        let p = parse_slash("/model claude-opus").unwrap();
        assert_eq!(p.name, "model");
        assert_eq!(p.args, "claude-opus");
    }

    #[test]
    fn parse_slash_no_args_yields_empty_args() {
        let p = parse_slash("/quit").unwrap();
        assert_eq!(p.name, "quit");
        assert_eq!(p.args, "");
    }

    #[test]
    fn parse_slash_non_slash_returns_none() {
        assert!(parse_slash("hello").is_none());
        assert!(parse_slash("/").is_none());
    }

    #[test]
    fn registry_get_finds_by_name() {
        let r = SlashCommandRegistry::new();
        assert!(r.get("help").is_some());
        assert!(r.get("/help").is_some());
        assert!(r.get("nope").is_none());
    }

    #[test]
    fn registry_marks_unimplemented_entries() {
        let r = SlashCommandRegistry::new();
        // `resume` ships unimplemented in 0.1.
        let resume = r.get("resume").expect("resume exists");
        assert!(!resume.implemented);
        let help = r.get("help").expect("help exists");
        assert!(help.implemented);
        // `tree` is wired now.
        let tree = r.get("tree").expect("tree exists");
        assert!(tree.implemented);
    }
}
