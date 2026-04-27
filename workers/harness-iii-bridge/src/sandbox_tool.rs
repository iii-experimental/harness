//! Sandboxed bash tool dispatching to the engine's built-in `iii-sandbox`
//! worker via `sandbox::exec`.
//!
//! Each call boots a microVM the first time and reuses it for subsequent
//! commands so that guest filesystem state carries across the agent's tool
//! calls. Use this in place of a host-process bash tool when you want
//! agent-driven shell commands isolated from the host.
//!
//! Requires the `iii-sandbox` worker to be running on the connected engine.
//! See iii's `docs/api-reference/sandbox.mdx` for catalog images, allowlist,
//! and CLI usage.

use std::sync::Arc;

use async_trait::async_trait;
use harness_runtime::ToolHandler;
use harness_types::{ContentBlock, TextContent, ToolCall, ToolResult};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::client::{BridgeError, IiiClientLike};

/// Default catalog image used when the caller doesn't pick one. The engine's
/// shipped catalog includes `python` and `node`; bash is available in either.
const DEFAULT_IMAGE: &str = "python";

/// Default idle timeout in seconds before the sandbox shuts down between
/// commands. Long enough to span a single agent run; the engine reaps stale
/// sandboxes automatically.
const DEFAULT_IDLE_TIMEOUT_SECS: u32 = 600;

/// Bash tool that runs commands inside an iii-sandbox microVM.
///
/// On first call, invokes `sandbox::create` with the configured image and
/// caches the returned `sandbox_id`. Subsequent calls reuse that sandbox via
/// `sandbox::exec`. Sandbox lifecycle is governed by the engine's idle
/// timeout; the tool does not call `sandbox::stop` itself so multi-turn
/// agent runs share working state.
pub struct SandboxedBashTool<C: IiiClientLike + 'static> {
    client: Arc<C>,
    image: String,
    idle_timeout_secs: u32,
    sandbox_id: Mutex<Option<String>>,
}

impl<C: IiiClientLike + 'static> SandboxedBashTool<C> {
    pub fn new(client: Arc<C>) -> Self {
        Self::with_image(client, DEFAULT_IMAGE)
    }

    pub fn with_image(client: Arc<C>, image: impl Into<String>) -> Self {
        Self {
            client,
            image: image.into(),
            idle_timeout_secs: DEFAULT_IDLE_TIMEOUT_SECS,
            sandbox_id: Mutex::new(None),
        }
    }

    pub fn with_idle_timeout(mut self, secs: u32) -> Self {
        self.idle_timeout_secs = secs;
        self
    }

    async fn ensure_sandbox(&self) -> Result<String, BridgeError> {
        let mut guard = self.sandbox_id.lock().await;
        if let Some(id) = guard.as_ref() {
            return Ok(id.clone());
        }
        let payload = json!({
            "image": self.image,
            "idle_timeout": self.idle_timeout_secs,
        });
        let response = self.client.invoke("sandbox::create", payload).await?;
        let id = response
            .get("sandbox_id")
            .and_then(Value::as_str)
            .ok_or_else(|| BridgeError::InvalidPayload {
                function_id: "sandbox::create".into(),
                reason: "missing sandbox_id".into(),
            })?
            .to_string();
        *guard = Some(id.clone());
        Ok(id)
    }
}

#[async_trait]
impl<C: IiiClientLike + 'static> ToolHandler for SandboxedBashTool<C> {
    async fn execute(&self, tool_call: &ToolCall) -> ToolResult {
        let command = tool_call
            .arguments
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if command.is_empty() {
            return error_result("missing required arg: command");
        }
        let sandbox_id = match self.ensure_sandbox().await {
            Ok(id) => id,
            Err(e) => return error_result(&format!("sandbox::create failed: {e}")),
        };
        let payload = json!({
            "sandbox_id": sandbox_id,
            "cmd": "bash",
            "args": ["-lc", command],
        });
        match self.client.invoke("sandbox::exec", payload).await {
            Ok(value) => render_exec_result(&value),
            Err(e) => error_result(&format!("sandbox::exec failed: {e}")),
        }
    }
}

fn render_exec_result(value: &Value) -> ToolResult {
    let stdout = value
        .get("stdout")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let stderr = value
        .get("stderr")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let exit_code = value.get("exit_code").and_then(Value::as_i64).unwrap_or(-1);

    use std::fmt::Write as _;
    let mut text = String::with_capacity(stdout.len() + stderr.len() + 16);
    let _ = writeln!(text, "exit={exit_code}");
    if !stdout.is_empty() {
        text.push_str(stdout);
    }
    if !stderr.is_empty() {
        if !stdout.is_empty() && !stdout.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(stderr);
    }

    ToolResult {
        content: vec![ContentBlock::Text(TextContent { text })],
        details: json!({ "exit_code": exit_code, "via": "iii-sandbox" }),
        terminate: false,
    }
}

fn error_result(message: &str) -> ToolResult {
    ToolResult {
        content: vec![ContentBlock::Text(TextContent {
            text: message.to_string(),
        })],
        details: json!({ "via": "iii-sandbox" }),
        terminate: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeClient;

    #[tokio::test]
    async fn first_call_creates_sandbox_then_execs() {
        let fake = Arc::new(FakeClient::new());
        fake.preset_invoke_response("sandbox::create", json!({ "sandbox_id": "sb-1" }))
            .await;
        fake.preset_invoke_response(
            "sandbox::exec",
            json!({ "stdout": "hello\n", "stderr": "", "exit_code": 0 }),
        )
        .await;

        let tool = SandboxedBashTool::new(fake.clone());
        let result = tool
            .execute(&ToolCall {
                id: "c1".into(),
                name: "bash".into(),
                arguments: json!({ "command": "echo hello" }),
            })
            .await;

        let text = match result.content.first().unwrap() {
            ContentBlock::Text(t) => &t.text,
            _ => panic!("expected text content"),
        };
        assert!(text.starts_with("exit=0"));
        assert!(text.contains("hello"));
        assert_eq!(result.details["exit_code"].as_i64(), Some(0));
        assert_eq!(result.details["via"].as_str(), Some("iii-sandbox"));
    }

    #[tokio::test]
    async fn empty_command_returns_error_without_invoking() {
        let fake = Arc::new(FakeClient::new());
        let tool = SandboxedBashTool::new(fake.clone());
        let result = tool
            .execute(&ToolCall {
                id: "c".into(),
                name: "bash".into(),
                arguments: json!({}),
            })
            .await;
        let text = match result.content.first().unwrap() {
            ContentBlock::Text(t) => &t.text,
            _ => panic!(),
        };
        assert!(text.contains("missing"));
    }

    #[tokio::test]
    async fn create_failure_surfaces_as_tool_error() {
        let fake = Arc::new(FakeClient::new());
        fake.preset_invoke_response("sandbox::create", json!({}))
            .await;
        let tool = SandboxedBashTool::new(fake.clone());
        let result = tool
            .execute(&ToolCall {
                id: "c".into(),
                name: "bash".into(),
                arguments: json!({ "command": "echo hi" }),
            })
            .await;
        let text = match result.content.first().unwrap() {
            ContentBlock::Text(t) => &t.text,
            _ => panic!(),
        };
        assert!(text.contains("missing sandbox_id"));
    }

    #[tokio::test]
    async fn second_call_reuses_cached_sandbox_id() {
        let fake = Arc::new(FakeClient::new());
        fake.preset_invoke_response("sandbox::create", json!({ "sandbox_id": "sb-cached" }))
            .await;
        fake.preset_invoke_response(
            "sandbox::exec",
            json!({ "stdout": "", "stderr": "", "exit_code": 0 }),
        )
        .await;

        let tool = SandboxedBashTool::new(fake.clone());
        for cmd in ["true", "echo two"] {
            let _ = tool
                .execute(&ToolCall {
                    id: "c".into(),
                    name: "bash".into(),
                    arguments: json!({ "command": cmd }),
                })
                .await;
        }

        let cached = tool.sandbox_id.lock().await;
        assert_eq!(cached.as_deref(), Some("sb-cached"));
    }

    #[test]
    fn render_includes_stdout_and_stderr() {
        let value = json!({ "stdout": "out\n", "stderr": "err\n", "exit_code": 1 });
        let result = render_exec_result(&value);
        let text = match result.content.first().unwrap() {
            ContentBlock::Text(t) => &t.text,
            _ => panic!(),
        };
        assert!(text.contains("exit=1"));
        assert!(text.contains("out"));
        assert!(text.contains("err"));
        assert!(!result.terminate);
    }
}
