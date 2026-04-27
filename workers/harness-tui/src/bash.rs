//! Inline `!cmd` and `!!cmd` helpers shared between the binary and tests.
//!
//! `!cmd`: run via bash, capture stdout+stderr, format as
//! `!cmd\n\n<output>`, and submit as a fresh user prompt.
//! `!!cmd`: same exec, but the formatted output is printed to scrollback as a
//! notification — never sent to the LLM.

/// Parsed inline-bash directive. `silent=true` for `!!`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineBash {
    pub silent: bool,
    pub command: String,
}

/// Parse an editor buffer into an `InlineBash` directive when the input begins
/// with `!`. Returns `None` for non-bash input.
pub fn parse(text: &str) -> Option<InlineBash> {
    if let Some(rest) = text.strip_prefix("!!") {
        return Some(InlineBash {
            silent: true,
            command: rest.trim().to_string(),
        });
    }
    if let Some(rest) = text.strip_prefix('!') {
        return Some(InlineBash {
            silent: false,
            command: rest.trim().to_string(),
        });
    }
    None
}

/// Cap to keep submitted prompts bounded.
pub const OUTPUT_CHAR_LIMIT: usize = 30_000;

/// Truncate `output` to the inline-bash char ceiling and format the message
/// the runtime sees as `!<cmd>\n\n<truncated>`.
pub fn format_for_submission(cmd: &str, output: &str) -> String {
    let truncated: String = output.chars().take(OUTPUT_CHAR_LIMIT).collect();
    format!("!{cmd}\n\n{truncated}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_double_bang_is_silent() {
        let p = parse("!!ls -la").expect("parse ok");
        assert!(p.silent);
        assert_eq!(p.command, "ls -la");
    }

    #[test]
    fn parse_single_bang_is_loud() {
        let p = parse("!echo hi").expect("parse ok");
        assert!(!p.silent);
        assert_eq!(p.command, "echo hi");
    }

    #[test]
    fn parse_non_bash_is_none() {
        assert!(parse("hello").is_none());
        assert!(parse("/help").is_none());
    }

    #[test]
    fn format_truncates_at_limit() {
        let big = "x".repeat(OUTPUT_CHAR_LIMIT + 5_000);
        let out = format_for_submission("ls", &big);
        // Header is "!ls\n\n" (5 chars) + at most OUTPUT_CHAR_LIMIT body chars.
        let body_len = out.chars().count() - "!ls\n\n".chars().count();
        assert_eq!(body_len, OUTPUT_CHAR_LIMIT);
    }

    #[test]
    fn format_preserves_short_output() {
        let out = format_for_submission("echo hi", "hi\n");
        assert_eq!(out, "!echo hi\n\nhi\n");
    }
}
