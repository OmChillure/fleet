//! Chat feature (`/chat`) — workspace AI chat (Grok / Ollama) with saved history.

mod api;
mod markdown;
mod page;

pub use api::{
    delete_conversation, fetch_chat_status, list_conversations, ChatModelInfo, ConversationSummary,
};
pub use page::{Chat, ChatThread};
