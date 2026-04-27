//! Session storage as a parent-id tree of typed entries.
//!
//! P0 surface: create / load / append / active_path / list / load_messages.
//! Fork / clone / compact / export_html land in P2.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use harness_types::{AgentContext, AgentMessage};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One entry in the session tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEntry {
    Message {
        id: String,
        parent_id: Option<String>,
        message: AgentMessage,
        timestamp: i64,
    },
    CustomMessage {
        id: String,
        parent_id: Option<String>,
        custom_type: String,
        content: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display: Option<String>,
        #[serde(default)]
        details: serde_json::Value,
        timestamp: i64,
    },
    BranchSummary {
        id: String,
        parent_id: Option<String>,
        summary: String,
        from_id: String,
        timestamp: i64,
    },
    Compaction {
        id: String,
        parent_id: Option<String>,
        summary: String,
        tokens_before: u64,
        details: CompactionDetails,
        timestamp: i64,
    },
}

impl SessionEntry {
    pub fn id(&self) -> &str {
        match self {
            Self::Message { id, .. }
            | Self::CustomMessage { id, .. }
            | Self::BranchSummary { id, .. }
            | Self::Compaction { id, .. } => id,
        }
    }

    pub fn parent_id(&self) -> Option<&str> {
        match self {
            Self::Message { parent_id, .. }
            | Self::CustomMessage { parent_id, .. }
            | Self::BranchSummary { parent_id, .. }
            | Self::Compaction { parent_id, .. } => parent_id.as_deref(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionDetails {
    #[serde(default)]
    pub read_files: Vec<String>,
    #[serde(default)]
    pub modified_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub branch_count: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("session not found: {0}")]
    NotFound(String),
    #[error("entry not found: {0}")]
    EntryNotFound(String),
    #[error("storage error: {0}")]
    Storage(String),
}

/// Storage backend abstraction.
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn create(&self, meta: SessionMeta) -> Result<(), SessionError>;
    async fn append(&self, session_id: &str, entry: SessionEntry) -> Result<(), SessionError>;
    async fn load_entries(&self, session_id: &str) -> Result<Vec<SessionEntry>, SessionError>;
    async fn load_meta(&self, session_id: &str) -> Result<SessionMeta, SessionError>;
    async fn list(&self) -> Result<Vec<SessionMeta>, SessionError>;
}

/// In-memory backend used by tests and replay tools.
#[derive(Debug, Clone, Default)]
pub struct InMemoryStore {
    entries: Arc<RwLock<HashMap<String, Vec<SessionEntry>>>>,
    meta: Arc<RwLock<HashMap<String, SessionMeta>>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SessionStore for InMemoryStore {
    async fn create(&self, meta: SessionMeta) -> Result<(), SessionError> {
        self.meta
            .write()
            .map_err(|e| SessionError::Storage(e.to_string()))?
            .insert(meta.session_id.clone(), meta.clone());
        self.entries
            .write()
            .map_err(|e| SessionError::Storage(e.to_string()))?
            .insert(meta.session_id, Vec::new());
        Ok(())
    }

    async fn append(&self, session_id: &str, entry: SessionEntry) -> Result<(), SessionError> {
        let mut entries = self
            .entries
            .write()
            .map_err(|e| SessionError::Storage(e.to_string()))?;
        let list = entries
            .get_mut(session_id)
            .ok_or_else(|| SessionError::NotFound(session_id.to_string()))?;
        list.push(entry);

        let mut meta = self
            .meta
            .write()
            .map_err(|e| SessionError::Storage(e.to_string()))?;
        if let Some(m) = meta.get_mut(session_id) {
            m.updated_at = chrono::Utc::now().timestamp_millis();
        }
        Ok(())
    }

    async fn load_entries(&self, session_id: &str) -> Result<Vec<SessionEntry>, SessionError> {
        self.entries
            .read()
            .map_err(|e| SessionError::Storage(e.to_string()))?
            .get(session_id)
            .cloned()
            .ok_or_else(|| SessionError::NotFound(session_id.to_string()))
    }

    async fn load_meta(&self, session_id: &str) -> Result<SessionMeta, SessionError> {
        self.meta
            .read()
            .map_err(|e| SessionError::Storage(e.to_string()))?
            .get(session_id)
            .cloned()
            .ok_or_else(|| SessionError::NotFound(session_id.to_string()))
    }

    async fn list(&self) -> Result<Vec<SessionMeta>, SessionError> {
        Ok(self
            .meta
            .read()
            .map_err(|e| SessionError::Storage(e.to_string()))?
            .values()
            .cloned()
            .collect())
    }
}

/// Create a new session and persist its meta. Returns the new session id.
pub async fn create_session<S: SessionStore + ?Sized>(
    store: &S,
    display_name: Option<String>,
    cwd: Option<String>,
) -> Result<String, SessionError> {
    let now = chrono::Utc::now().timestamp_millis();
    let session_id = Uuid::new_v4().to_string();
    store
        .create(SessionMeta {
            session_id: session_id.clone(),
            display_name,
            created_at: now,
            updated_at: now,
            cwd,
            branch_count: 1,
        })
        .await?;
    Ok(session_id)
}

/// Append a message entry, deriving id and timestamp.
pub async fn append_message<S: SessionStore + ?Sized>(
    store: &S,
    session_id: &str,
    parent_id: Option<String>,
    message: AgentMessage,
) -> Result<String, SessionError> {
    let id = Uuid::new_v4().to_string();
    let entry = SessionEntry::Message {
        id: id.clone(),
        parent_id,
        message,
        timestamp: chrono::Utc::now().timestamp_millis(),
    };
    store.append(session_id, entry).await?;
    Ok(id)
}

/// Active path from root to leaf. If `leaf` is None, walks back from the most
/// recently appended entry. Returns entry ids in root-first order.
pub async fn active_path<S: SessionStore + ?Sized>(
    store: &S,
    session_id: &str,
    leaf: Option<&str>,
) -> Result<Vec<String>, SessionError> {
    let entries = store.load_entries(session_id).await?;
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let by_id: HashMap<&str, &SessionEntry> = entries.iter().map(|e| (e.id(), e)).collect();
    let leaf_id = match leaf {
        Some(id) => id,
        None => entries.last().expect("non-empty checked").id(),
    };
    let mut path: Vec<String> = Vec::new();
    let mut cursor: Option<&str> = Some(leaf_id);
    while let Some(id) = cursor {
        path.push(id.to_string());
        cursor = by_id.get(id).and_then(|e| e.parent_id());
    }
    path.reverse();
    Ok(path)
}

/// Build an `AgentContext` from the active path's message entries.
pub async fn load_messages<S: SessionStore + ?Sized>(
    store: &S,
    session_id: &str,
    leaf: Option<&str>,
) -> Result<Vec<AgentMessage>, SessionError> {
    let entries = store.load_entries(session_id).await?;
    let path = active_path(store, session_id, leaf).await?;
    let by_id: HashMap<&str, &SessionEntry> = entries.iter().map(|e| (e.id(), e)).collect();
    let mut messages: Vec<AgentMessage> = Vec::new();
    for id in &path {
        if let Some(SessionEntry::Message { message, .. }) = by_id.get(id.as_str()).copied() {
            messages.push(message.clone());
        }
    }
    Ok(messages)
}

/// Hydrate an `AgentContext` from a session leaf using a system prompt.
pub async fn load_context<S: SessionStore + ?Sized>(
    store: &S,
    session_id: &str,
    leaf: Option<&str>,
    system_prompt: String,
) -> Result<AgentContext, SessionError> {
    let messages = load_messages(store, session_id, leaf).await?;
    Ok(AgentContext {
        system_prompt,
        messages,
        tools: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_types::{ContentBlock, TextContent, UserMessage};

    fn user(text: &str, ts: i64) -> AgentMessage {
        AgentMessage::User(UserMessage {
            content: vec![ContentBlock::Text(TextContent {
                text: text.to_string(),
            })],
            timestamp: ts,
        })
    }

    #[tokio::test]
    async fn create_then_load_meta() {
        let store = InMemoryStore::new();
        let id = create_session(&store, Some("test".into()), None)
            .await
            .unwrap();
        let meta = store.load_meta(&id).await.unwrap();
        assert_eq!(meta.session_id, id);
        assert_eq!(meta.display_name, Some("test".into()));
    }

    #[tokio::test]
    async fn append_and_active_path_linear() {
        let store = InMemoryStore::new();
        let id = create_session(&store, None, None).await.unwrap();
        let a = append_message(&store, &id, None, user("a", 1))
            .await
            .unwrap();
        let b = append_message(&store, &id, Some(a.clone()), user("b", 2))
            .await
            .unwrap();
        let c = append_message(&store, &id, Some(b.clone()), user("c", 3))
            .await
            .unwrap();
        let path = active_path(&store, &id, None).await.unwrap();
        assert_eq!(path, vec![a, b, c]);
    }

    #[tokio::test]
    async fn active_path_at_leaf_branches() {
        let store = InMemoryStore::new();
        let id = create_session(&store, None, None).await.unwrap();
        let a = append_message(&store, &id, None, user("a", 1))
            .await
            .unwrap();
        let b = append_message(&store, &id, Some(a.clone()), user("b", 2))
            .await
            .unwrap();
        // branch from a
        let c = append_message(&store, &id, Some(a.clone()), user("c", 3))
            .await
            .unwrap();
        let path_b = active_path(&store, &id, Some(&b)).await.unwrap();
        let path_c = active_path(&store, &id, Some(&c)).await.unwrap();
        assert_eq!(path_b, vec![a.clone(), b]);
        assert_eq!(path_c, vec![a, c]);
    }

    #[tokio::test]
    async fn load_messages_returns_path_messages() {
        let store = InMemoryStore::new();
        let id = create_session(&store, None, None).await.unwrap();
        append_message(&store, &id, None, user("hello", 1))
            .await
            .unwrap();
        let msgs = load_messages(&store, &id, None).await.unwrap();
        assert_eq!(msgs.len(), 1);
    }

    #[tokio::test]
    async fn list_returns_all_sessions() {
        let store = InMemoryStore::new();
        create_session(&store, Some("a".into()), None)
            .await
            .unwrap();
        create_session(&store, Some("b".into()), None)
            .await
            .unwrap();
        let listed = store.list().await.unwrap();
        assert_eq!(listed.len(), 2);
    }
}
