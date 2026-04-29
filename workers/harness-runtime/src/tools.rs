//! Built-in tool handlers inlined in the runtime for performance.
//!
//! Each tool implements [`crate::runtime::ToolHandler`]. Bash dispatches to the
//! engine's built-in `shell` worker via iii.trigger when wired up; the
//! placeholder here returns a not-implemented error to keep the loop honest.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use harness_types::{ContentBlock, TextContent, ToolCall, ToolResult};

use crate::runtime::ToolHandler;

fn ok(text: impl Into<String>) -> ToolResult {
    ToolResult {
        content: vec![ContentBlock::Text(TextContent { text: text.into() })],
        details: serde_json::json!({}),
        terminate: false,
    }
}

fn err(text: impl Into<String>) -> ToolResult {
    ToolResult {
        content: vec![ContentBlock::Text(TextContent { text: text.into() })],
        details: serde_json::json!({}),
        terminate: false,
    }
}

fn arg_string(args: &serde_json::Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(ToString::to_string)
}

fn arg_path(args: &serde_json::Value, key: &str) -> Option<PathBuf> {
    arg_string(args, key).map(PathBuf::from)
}

/// `read` function — read a file's contents. Args: `{ path: string, max_bytes?: u64 }`.
pub struct ReadTool;

#[async_trait]
impl ToolHandler for ReadTool {
    async fn execute(&self, tool_call: &ToolCall) -> ToolResult {
        let Some(path) = arg_path(&tool_call.arguments, "path") else {
            return err("missing required arg: path");
        };
        match tokio::fs::read(&path).await {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(s) => ok(s),
                Err(e) => err(format!("not utf-8: {e}")),
            },
            Err(e) => err(format!("read failed: {e}")),
        }
    }
}

/// `write` function — write a file. Args: `{ path: string, content: string }`.
pub struct WriteTool;

#[async_trait]
impl ToolHandler for WriteTool {
    async fn execute(&self, tool_call: &ToolCall) -> ToolResult {
        let Some(path) = arg_path(&tool_call.arguments, "path") else {
            return err("missing required arg: path");
        };
        let Some(content) = arg_string(&tool_call.arguments, "content") else {
            return err("missing required arg: content");
        };
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(e) = tokio::fs::create_dir_all(parent).await {
                    return err(format!("mkdir failed: {e}"));
                }
            }
        }
        match tokio::fs::write(&path, content).await {
            Ok(()) => ok(format!("wrote {}", path.display())),
            Err(e) => err(format!("write failed: {e}")),
        }
    }
}

/// `edit` function — replace `old_string` with `new_string` in a file.
///
/// Args: `{ path: string, old_string: string, new_string: string }`. Fails if
/// the old string is not present or appears more than once (caller must add
/// context to disambiguate).
pub struct EditTool;

#[async_trait]
impl ToolHandler for EditTool {
    async fn execute(&self, tool_call: &ToolCall) -> ToolResult {
        let Some(path) = arg_path(&tool_call.arguments, "path") else {
            return err("missing required arg: path");
        };
        let Some(old) = arg_string(&tool_call.arguments, "old_string") else {
            return err("missing required arg: old_string");
        };
        let Some(new) = arg_string(&tool_call.arguments, "new_string") else {
            return err("missing required arg: new_string");
        };
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) => return err(format!("read failed: {e}")),
        };
        let Ok(text) = String::from_utf8(bytes) else {
            return err("file is not utf-8");
        };
        let count = text.matches(&old).count();
        if count == 0 {
            return err("old_string not found");
        }
        if count > 1 {
            return err(format!(
                "old_string matched {count} times; provide more context"
            ));
        }
        let updated = text.replacen(&old, &new, 1);
        match tokio::fs::write(&path, updated).await {
            Ok(()) => ok(format!("edited {}", path.display())),
            Err(e) => err(format!("write failed: {e}")),
        }
    }
}

/// `ls` function — list directory entries. Args: `{ path: string }`.
pub struct LsTool;

#[async_trait]
impl ToolHandler for LsTool {
    async fn execute(&self, tool_call: &ToolCall) -> ToolResult {
        let Some(path) = arg_path(&tool_call.arguments, "path") else {
            return err("missing required arg: path");
        };
        let mut entries = match tokio::fs::read_dir(&path).await {
            Ok(rd) => rd,
            Err(e) => return err(format!("readdir failed: {e}")),
        };
        let mut names: Vec<String> = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
        names.sort();
        ok(names.join("\n"))
    }
}

/// `find` function — find files by name suffix. Args: `{ root: string, suffix: string }`.
/// Walks recursively up to a fixed depth to keep the contract simple.
pub struct FindTool;

#[async_trait]
impl ToolHandler for FindTool {
    async fn execute(&self, tool_call: &ToolCall) -> ToolResult {
        let Some(root) = arg_path(&tool_call.arguments, "root") else {
            return err("missing required arg: root");
        };
        let Some(suffix) = arg_string(&tool_call.arguments, "suffix") else {
            return err("missing required arg: suffix");
        };
        let mut hits: Vec<String> = Vec::new();
        let mut stack = vec![(root.clone(), 0u32)];
        while let Some((dir, depth)) = stack.pop() {
            if depth > 8 {
                continue;
            }
            let mut entries = match tokio::fs::read_dir(&dir).await {
                Ok(rd) => rd,
                Err(_) => continue,
            };
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                let Ok(meta) = entry.metadata().await else {
                    continue;
                };
                if meta.is_dir() {
                    stack.push((path, depth + 1));
                } else if path.to_string_lossy().ends_with(&suffix) {
                    hits.push(path.to_string_lossy().to_string());
                }
            }
        }
        hits.sort();
        ok(hits.join("\n"))
    }
}

/// `grep` function — substring search across files in a directory. Args:
/// `{ root: string, pattern: string }`. Reports `path:line_no:line` per match.
pub struct GrepTool;

#[async_trait]
impl ToolHandler for GrepTool {
    async fn execute(&self, tool_call: &ToolCall) -> ToolResult {
        let Some(root) = arg_path(&tool_call.arguments, "root") else {
            return err("missing required arg: root");
        };
        let Some(pattern) = arg_string(&tool_call.arguments, "pattern") else {
            return err("missing required arg: pattern");
        };
        let mut hits: Vec<String> = Vec::new();
        let mut stack = vec![(root.clone(), 0u32)];
        while let Some((dir, depth)) = stack.pop() {
            if depth > 8 {
                continue;
            }
            let mut entries = match tokio::fs::read_dir(&dir).await {
                Ok(rd) => rd,
                Err(_) => continue,
            };
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                let Ok(meta) = entry.metadata().await else {
                    continue;
                };
                if meta.is_dir() {
                    stack.push((path, depth + 1));
                    continue;
                }
                if let Ok(bytes) = tokio::fs::read(&path).await {
                    if let Ok(text) = String::from_utf8(bytes) {
                        for (i, line) in text.lines().enumerate() {
                            if line.contains(&pattern) {
                                hits.push(format!("{}:{}:{}", path.display(), i + 1, line));
                            }
                        }
                    }
                }
            }
        }
        hits.sort();
        ok(hits.join("\n"))
    }
}

/// `bash` placeholder.
///
/// Production runtime dispatches to engine `shell` via iii.trigger; this
/// default returns a not-implemented error so consumers can detect when a
/// session hits bash without a wired runtime.
pub struct BashPlaceholder;

#[async_trait]
impl ToolHandler for BashPlaceholder {
    async fn execute(&self, _tool_call: &ToolCall) -> ToolResult {
        ToolResult {
            content: vec![ContentBlock::Text(TextContent {
                text: "bash function not wired; bind iii-sandbox or host shell".into(),
            })],
            details: serde_json::json!({ "wired": false }),
            terminate: false,
        }
    }
}

/// Helper: build an [`Path`] argument for fixtures and tests.
pub fn arg_path_for_test(p: &Path) -> serde_json::Value {
    serde_json::json!({ "path": p.to_string_lossy() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_types::ToolCall;
    use std::sync::Arc;
    use tempfile_minimal::tempdir;

    #[tokio::test]
    async fn read_then_write_then_edit_roundtrip() {
        let dir = tempdir();
        let p = dir.path().join("a.txt");
        let writer: Arc<dyn ToolHandler> = Arc::new(WriteTool);
        let reader: Arc<dyn ToolHandler> = Arc::new(ReadTool);
        let editor: Arc<dyn ToolHandler> = Arc::new(EditTool);

        let _ = writer
            .execute(&ToolCall {
                id: "w1".into(),
                name: "write".into(),
                arguments: serde_json::json!({
                    "path": p.to_string_lossy(),
                    "content": "hello world",
                }),
            })
            .await;
        let read_result = reader
            .execute(&ToolCall {
                id: "r1".into(),
                name: "read".into(),
                arguments: serde_json::json!({ "path": p.to_string_lossy() }),
            })
            .await;
        if let Some(ContentBlock::Text(t)) = read_result.content.first() {
            assert_eq!(t.text, "hello world");
        } else {
            panic!("expected text content");
        }
        let _ = editor
            .execute(&ToolCall {
                id: "e1".into(),
                name: "edit".into(),
                arguments: serde_json::json!({
                    "path": p.to_string_lossy(),
                    "old_string": "world",
                    "new_string": "harness",
                }),
            })
            .await;
        let read2 = reader
            .execute(&ToolCall {
                id: "r2".into(),
                name: "read".into(),
                arguments: serde_json::json!({ "path": p.to_string_lossy() }),
            })
            .await;
        if let Some(ContentBlock::Text(t)) = read2.content.first() {
            assert_eq!(t.text, "hello harness");
        }
    }

    #[tokio::test]
    async fn ls_lists_entries() {
        let dir = tempdir();
        tokio::fs::write(dir.path().join("a"), b"").await.unwrap();
        tokio::fs::write(dir.path().join("b"), b"").await.unwrap();
        let ls: Arc<dyn ToolHandler> = Arc::new(LsTool);
        let result = ls
            .execute(&ToolCall {
                id: "l1".into(),
                name: "ls".into(),
                arguments: serde_json::json!({ "path": dir.path().to_string_lossy() }),
            })
            .await;
        if let Some(ContentBlock::Text(t)) = result.content.first() {
            assert!(t.text.contains('a'));
            assert!(t.text.contains('b'));
        }
    }

    #[tokio::test]
    async fn edit_fails_on_ambiguous_match() {
        let dir = tempdir();
        let p = dir.path().join("dup.txt");
        tokio::fs::write(&p, b"foo foo").await.unwrap();
        let editor: Arc<dyn ToolHandler> = Arc::new(EditTool);
        let result = editor
            .execute(&ToolCall {
                id: "e1".into(),
                name: "edit".into(),
                arguments: serde_json::json!({
                    "path": p.to_string_lossy(),
                    "old_string": "foo",
                    "new_string": "bar",
                }),
            })
            .await;
        let text = match result.content.first().unwrap() {
            ContentBlock::Text(t) => &t.text,
            _ => panic!(),
        };
        assert!(text.contains("matched 2 times"));
    }

    /// Tiny tempdir helper to avoid pulling tempfile crate at this stage.
    mod tempfile_minimal {
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicU64, Ordering};

        pub struct TempDir {
            path: PathBuf,
        }

        impl TempDir {
            pub fn path(&self) -> &PathBuf {
                &self.path
            }
        }

        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.path);
            }
        }

        static COUNTER: AtomicU64 = AtomicU64::new(0);

        pub fn tempdir() -> TempDir {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let path =
                std::env::temp_dir().join(format!("harness-test-{}-{}", std::process::id(), n));
            std::fs::create_dir_all(&path).unwrap();
            TempDir { path }
        }
    }
}
