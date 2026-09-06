//! Chat history (`/{ws}/sessions`) vs worker live view (`/{ws}/sessions/{id}`).
//!
//! Docs §6.3: the list is saved chats (`GET …/chat/conversations`). A row
//! opens `/{ws}/chat/{conversation_id}`.
//! Docs §6.4: the worker live view is `/{ws}/sessions/{session_id}` and is
//! reached from Fleet session cards, not this list.

/// Sidebar + page heading for `/{ws}/sessions`.
pub const PAGE_HEADING: &str = "Chat history";
pub const PAGE_SUBTITLE: &str = "Saved conversations for this workspace. Open one to continue.";
pub const EMPTY_COPY: &str = "No saved chats yet. Start a conversation and it will appear here.";
pub const NEW_CHAT_LABEL: &str = "New chat";
pub const GO_TO_CHAT_LABEL: &str = "Go to Chat";

/// Worker live view (`/{ws}/sessions/{session_id}`) chrome.
pub const LIVE_SESSION_HEADING: &str = "Live session";
pub const BACK_TO_HISTORY_LABEL: &str = "← All sessions";

/// `/{workspace_id}/sessions` — chat history list.
pub fn chat_history_path(workspace_id: &str) -> String {
    format!("/{workspace_id}/sessions")
}

/// `/{workspace_id}/chat/{conversation_id}` — row click target.
pub fn chat_thread_path(workspace_id: &str, conversation_id: &str) -> String {
    format!("/{workspace_id}/chat/{conversation_id}")
}

/// `/{workspace_id}/sessions/{session_id}` — Fleet card live view.
pub fn worker_live_path(workspace_id: &str, session_id: &str) -> String {
    format!("/{workspace_id}/sessions/{session_id}")
}

/// GET one worker session (live view poll). Not the chat-history list.
pub fn worker_session_detail_path(workspace_id: &str, session_id: &str) -> String {
    format!("/api/workspaces/{workspace_id}/fleet/sessions/{session_id}")
}
