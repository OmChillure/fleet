//! Chat history (`/{ws}/sessions`) vs worker live view (`/{ws}/sessions/{id}`).
//!
//! Docs §6.3: the list is saved chats (`GET …/chat/conversations`). A row
//! opens `/{ws}/chat/{conversation_id}`.
//! Docs §6.4: the worker live view is `/{ws}/sessions/{session_id}` and is
//! reached from Fleet session cards, not this list.

use serde::Deserialize;
use serde_json::Value;

/// Sidebar + page heading for `/{ws}/sessions`.
pub const PAGE_HEADING: &str = "Chat history";
pub const PAGE_SUBTITLE: &str = "Saved conversations for this workspace. Open one to continue.";
pub const EMPTY_COPY: &str = "No saved chats yet. Start a conversation and it will appear here.";
pub const NEW_CHAT_LABEL: &str = "New chat";
pub const GO_TO_CHAT_LABEL: &str = "Go to Chat";

/// Sidebar labels that must open chat history, not worker sandboxes.
pub const CHAT_HISTORY_NAV_LABEL: &str = "Chat history";
pub const LIVE_SESSIONS_NAV_LABEL: &str = "Live sessions";

/// Worker live view (`/{ws}/sessions/{session_id}`) chrome.
pub const LIVE_SESSION_HEADING: &str = "Live session";
pub const BACK_TO_HISTORY_LABEL: &str = "← All sessions";

/// JSON keys required on a chat-history row (GET `/chat/conversations` item).
pub const CHAT_ROW_REQUIRED_KEYS: &[&str] =
    &["id", "workspace_id", "title", "created_at", "updated_at"];

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ChatHistoryRow {
    pub id: String,
    pub workspace_id: String,
    pub title: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub message_count: i64,
    #[serde(default)]
    pub preview: Option<String>,
}

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

/// GET list used by the chat-history page.
pub fn list_conversations_path(workspace_id: &str) -> String {
    format!("/api/workspaces/{workspace_id}/chat/conversations")
}

/// GET one worker session (live view poll). Not the chat-history list.
pub fn worker_session_detail_path(workspace_id: &str, session_id: &str) -> String {
    format!("/api/workspaces/{workspace_id}/fleet/sessions/{session_id}")
}

/// GET worker-session list (Fleet / Sandboxes). Chat history must not call this.
pub fn list_worker_sessions_path(workspace_id: &str) -> String {
    format!("/api/workspaces/{workspace_id}/fleet/sessions")
}

/// Docs §6.3: clicking a history row opens the chat thread.
pub fn row_opens_chat_thread(path: &str, workspace_id: &str, conversation_id: &str) -> bool {
    path == chat_thread_path(workspace_id, conversation_id)
}

/// Docs §6.4: worker live view is a different URL from a chat thread.
pub fn row_opens_worker_live(path: &str, workspace_id: &str, id: &str) -> bool {
    path == worker_live_path(workspace_id, id)
}

/// Fleet board (`/{workspace_id}/fleet`). Live-view “← All sessions” must not go here.
pub fn fleet_board_path(workspace_id: &str) -> String {
    format!("/{workspace_id}/fleet")
}

/// Docs §6.4: live-view “← All sessions” lands on Chat history, never the fleet board.
pub fn live_session_back_path(workspace_id: &str) -> String {
    chat_history_path(workspace_id)
}

pub fn parse_chat_row(value: &Value) -> Result<ChatHistoryRow, serde_json::Error> {
    serde_json::from_value(value.clone())
}

/// Sidebar "Live sessions" is the same route as Chat history (`/{ws}/sessions`).
#[cfg(test)]
pub fn live_sessions_nav_path(workspace_id: &str) -> String {
    chat_history_path(workspace_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // catalog:list-sessions-page-lists-chats-not-workers
    // catalog:list-session-live-back-link-to-chats — §6.4 ← All sessions → history, not /fleet.
    // catalog:unit-sessions-page — /sessions lists chats; click → /chat/{id}.
    // Host: cargo test -p fleet-ui --locked -- sessions
    // Layer-2 page lock (just test excludes fleet-ui):
    //   python3 -m pytest tests/browser/test_app_e2e.py -q -k sessions_page_lists_chats

    const WS: &str = "7f3a1c2e-9b04-4d55-8e11-0a6b2c4d8e90";
    const CID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const SID: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

    fn chat_item() -> Value {
        json!({
            "id": CID,
            "workspace_id": WS,
            "title": "Hello from chat",
            "preview": "hi",
            "message_count": 3,
            "provider": "grok",
            "model": "grok-4.5",
            "created_at": "2026-08-25T12:00:00Z",
            "updated_at": "2026-08-25T12:01:00Z",
        })
    }

    fn worker_item() -> Value {
        json!({
            "id": SID,
            "run_id": "11111111-1111-4111-8111-111111111111",
            "board_task_id": "33333333-3333-4333-8333-333333333333",
            "attempt": 1,
            "status": "healthy",
            "runtime_backend": "process",
            "worker_profile": "coding",
            "progress_pct": 40,
            "work_dir": "/tmp/work/s1",
            "started_at": "2026-08-25T12:00:00Z",
        })
    }

    fn production_page() -> &'static str {
        include_str!("page.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("sessions page.rs")
    }

    fn sessions_list_src(page: &str) -> &str {
        let start = page
            .find("pub fn Sessions")
            .expect("Sessions list component");
        let end = page[start..]
            .find("pub fn SessionDetail")
            .map(|i| start + i)
            .unwrap_or(page.len());
        &page[start..end]
    }

    fn session_detail_src(page: &str) -> &str {
        let start = page
            .find("pub fn SessionDetail")
            .expect("SessionDetail component");
        &page[start..]
    }

    /// The dioxus `Link` whose child is `{BACK_TO_HISTORY_LABEL}`.
    fn session_detail_back_link_src(detail: &str) -> &str {
        const LABEL: &str = "{BACK_TO_HISTORY_LABEL}";
        let n = detail.matches(LABEL).count();
        assert_eq!(
            n, 1,
            "SessionDetail must render exactly one {{BACK_TO_HISTORY_LABEL}}, found {n}"
        );
        let label_at = detail
            .find(LABEL)
            .expect("SessionDetail must render {BACK_TO_HISTORY_LABEL}");
        let link_at = detail[..label_at].rfind("Link {").unwrap_or_else(|| {
            panic!(
                "docs §6.4: ← All sessions must be a dioxus Link, not a raw href / button; \
                 window={:?}",
                &detail[label_at.saturating_sub(160)..(label_at + LABEL.len()).min(detail.len())]
            )
        });
        let mut depth = 0i32;
        let mut end = None;
        for (idx, ch) in detail[link_at..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(link_at + idx + ch.len_utf8());
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = end.expect("unbalanced Link { for ← All sessions");
        assert!(
            link_at < label_at && label_at < end,
            "BACK_TO_HISTORY_LABEL must be a child of the back Link"
        );
        &detail[link_at..end]
    }

    fn nav_route_for_label(sidebar: &str, label: &str) -> &'static str {
        let needle = format!("label: \"{label}\"");
        let idx = sidebar
            .find(&needle)
            .unwrap_or_else(|| panic!("sidebar missing label {label:?}"));
        let window = &sidebar[idx.saturating_sub(220)..idx];
        if window.contains("Route::Sessions") {
            "Sessions"
        } else if window.contains("Route::Sandboxes") {
            "Sandboxes"
        } else if window.contains("Route::SessionDetail") {
            "SessionDetail"
        } else if window.contains("Route::ChatThread") {
            "ChatThread"
        } else if window.contains("Route::Fleet") {
            "Fleet"
        } else {
            "other"
        }
    }

    #[test]
    fn chat_history_paths_are_not_worker_live() {
        assert_eq!(chat_history_path(WS), format!("/{WS}/sessions"));
        assert_eq!(chat_thread_path(WS, CID), format!("/{WS}/chat/{CID}"));
        assert_eq!(worker_live_path(WS, SID), format!("/{WS}/sessions/{SID}"));
        assert_ne!(chat_history_path(WS), worker_live_path(WS, SID));
        assert_ne!(chat_thread_path(WS, CID), worker_live_path(WS, CID));
        assert_ne!(chat_thread_path(WS, CID), chat_history_path(WS));
        assert_eq!(live_sessions_nav_path(WS), chat_history_path(WS));
        assert_ne!(live_sessions_nav_path(WS), format!("/{WS}/sandboxes"));
    }

    #[test]
    fn row_click_opens_chat_thread_not_worker_live() {
        let open = chat_thread_path(WS, CID);
        assert!(row_opens_chat_thread(&open, WS, CID));
        assert!(!row_opens_worker_live(&open, WS, CID));
        let live = worker_live_path(WS, SID);
        assert!(row_opens_worker_live(&live, WS, SID));
        assert!(!row_opens_chat_thread(&live, WS, SID));
        assert!(!row_opens_chat_thread(&chat_history_path(WS), WS, CID));
    }

    #[test]
    fn list_api_is_conversations_not_fleet_sessions() {
        let chats = list_conversations_path(WS);
        let workers = list_worker_sessions_path(WS);
        let detail = worker_session_detail_path(WS, SID);
        assert_eq!(chats, format!("/api/workspaces/{WS}/chat/conversations"));
        assert_eq!(workers, format!("/api/workspaces/{WS}/fleet/sessions"));
        assert_eq!(detail, format!("/api/workspaces/{WS}/fleet/sessions/{SID}"));
        assert_ne!(chats, workers);
        assert!(!chats.contains("/fleet/sessions"));
        assert!(!chats.contains("/fleet/workers"));
        assert!(!workers.contains("/chat/conversations"));
        assert!(detail.starts_with(&format!("{workers}/")));
    }

    #[test]
    fn chat_history_item_parses_worker_session_does_not() {
        let row = parse_chat_row(&chat_item()).expect("chat row");
        assert_eq!(row.id, CID);
        assert_eq!(row.title, "Hello from chat");
        assert_eq!(row.message_count, 3);
        assert!(
            parse_chat_row(&worker_item()).is_err(),
            "fleet worker session JSON must not parse as a chat-history row"
        );
        for key in CHAT_ROW_REQUIRED_KEYS {
            assert!(chat_item().get(*key).is_some(), "chat row missing {key}");
        }
    }

    #[test]
    fn page_copy_is_chat_history_not_sandboxes() {
        assert_eq!(PAGE_HEADING, "Chat history");
        assert!(PAGE_SUBTITLE.contains("Saved conversations"));
        assert!(EMPTY_COPY.contains("No saved chats yet"));
        assert_eq!(NEW_CHAT_LABEL, "New chat");
        assert_eq!(LIVE_SESSION_HEADING, "Live session");
        assert_ne!(PAGE_HEADING, "Sandboxes");
        assert_ne!(PAGE_HEADING, "Sessions");
        assert!(!PAGE_SUBTITLE.to_ascii_lowercase().contains("worker"));
        assert!(!EMPTY_COPY.to_ascii_lowercase().contains("sandbox"));
    }

    #[test]
    fn sessions_list_rsx_loads_conversations_and_opens_chat() {
        let page = production_page();
        let list = sessions_list_src(page);
        assert!(
            list.contains("list_conversations"),
            "Sessions must GET chat conversations, not fleet sessions"
        );
        assert!(list.contains("{PAGE_HEADING}"));
        assert!(list.contains("{PAGE_SUBTITLE}"));
        assert!(list.contains("{EMPTY_COPY}"));
        assert!(list.contains("{NEW_CHAT_LABEL}"));
        assert!(
            list.contains("Route::ChatThread"),
            "row click must push /chat/{{id}}"
        );
        assert!(
            list.contains("conversation_id: id.clone()"),
            "row click must use the conversation id"
        );
        assert!(
            !list.contains("get_session_detail"),
            "chat history must not poll fleet session detail"
        );
        assert!(
            !list.contains("list_sessions"),
            "chat history must not list fleet worker sessions"
        );
        assert!(
            !list.contains("Route::SessionDetail"),
            "chat history rows must not open the worker live view"
        );
        assert!(!list.contains("sandbox session"));
        assert!(!list.contains("Kill"));
    }

    #[test]
    fn session_detail_rsx_is_fleet_live_view() {
        let page = production_page();
        let detail = session_detail_src(page);
        assert!(
            detail.contains("get_session_detail"),
            "live view must poll GET /fleet/sessions/{{id}}"
        );
        assert!(detail.contains("{LIVE_SESSION_HEADING}"));
        assert!(detail.contains("{BACK_TO_HISTORY_LABEL}"));
        assert!(
            detail.contains("Route::Sessions"),
            "← All sessions must return to chat history"
        );
        assert!(
            !detail.contains("list_conversations"),
            "worker live view must not load saved chats"
        );
        assert!(!detail.contains("Route::ChatThread"));
        assert!(
            !detail.contains("Route::Fleet"),
            "live view must not link to the fleet board"
        );
    }

    #[test]
    fn live_session_back_link_goes_to_chat_history_not_fleet() {
        // catalog:list-session-live-back-link-to-chats
        // Docs §6.4: “← All sessions” goes to Chat history (`/{ws}/sessions`),
        // not the fleet board (`/{ws}/fleet`). Easy to wire to Route::Fleet.
        assert_eq!(BACK_TO_HISTORY_LABEL, "← All sessions");
        assert_eq!(live_session_back_path(WS), chat_history_path(WS));
        assert_eq!(live_session_back_path(WS), format!("/{WS}/sessions"));
        assert_eq!(fleet_board_path(WS), format!("/{WS}/fleet"));
        assert_ne!(live_session_back_path(WS), fleet_board_path(WS));
        assert!(
            !live_session_back_path(WS).contains("/fleet"),
            "back path must stay on /sessions, not /fleet"
        );
        assert_ne!(worker_live_path(WS, SID), fleet_board_path(WS));

        let detail = session_detail_src(production_page());
        let link = session_detail_back_link_src(detail);
        assert!(
            link.contains("to: Route::Sessions"),
            "docs §6.4: ← All sessions must navigate to Route::Sessions \
             (/{{ws}}/sessions chat history), got {link}"
        );
        assert!(
            link.contains("workspace_id: workspace_id.clone()"),
            "back link must stay in the same workspace, got {link}"
        );
        assert!(
            link.contains("{BACK_TO_HISTORY_LABEL}"),
            "back Link child must be BACK_TO_HISTORY_LABEL, got {link}"
        );
        for forbidden in [
            "Route::Fleet",
            "Route::Dashboard",
            "Route::Sandboxes",
            "Route::Home",
            "Route::Chat ",
            "Route::ChatThread",
            "Route::Chat {",
            "/fleet",
            "href:",
        ] {
            assert!(
                !link.contains(forbidden),
                "docs §6.4: ← All sessions must not go to {forbidden} \
                 (chat history, not the fleet board). link={link}"
            );
        }
        assert!(
            !detail.contains("Route::Fleet"),
            "SessionDetail must not mention Route::Fleet at all"
        );
        assert!(
            !detail.contains("/fleet"),
            "SessionDetail must not hard-code /fleet as a back target"
        );
    }

    #[test]
    fn session_detail_client_is_fleet_not_chat_list() {
        let api = include_str!("api.rs");
        assert!(
            api.contains("worker_session_detail_path"),
            "detail client must use worker_session_detail_path"
        );
        assert!(
            !api.contains("list_conversations_path"),
            "session detail client is not the chat list"
        );
        assert!(
            !api.contains("/chat/conversations"),
            "session detail client must not hit chat conversations"
        );
    }

    #[test]
    fn chat_list_client_hits_conversations() {
        let chat_api = include_str!("../chat/api.rs");
        assert!(
            chat_api.contains("/api/workspaces/{workspace_id}/chat/conversations"),
            "list_conversations must GET /chat/conversations"
        );
        let list_fn = chat_api
            .split("pub async fn list_conversations")
            .nth(1)
            .and_then(|rest| rest.split("pub async fn create_conversation").next())
            .expect("list_conversations");
        assert!(
            list_fn.contains("/chat/conversations"),
            "list_conversations path must stay chat conversations"
        );
        assert!(
            !list_fn.contains("/fleet/sessions"),
            "list_conversations must not GET fleet worker sessions"
        );
    }

    #[test]
    fn fleet_cards_open_worker_live_view() {
        let fleet = include_str!("../fleet/page.rs");
        assert!(
            fleet.contains("Route::SessionDetail"),
            "Fleet session cards must open /sessions/{{session_id}}"
        );
        assert!(
            fleet.contains("session_id: s.id.clone()"),
            "Fleet Open must pass the worker session id"
        );
        let open = fleet.split("\"Open\"").next().expect("Fleet Open link");
        assert!(
            open.contains("Route::SessionDetail"),
            "Open on a Fleet card is the worker live view, not /chat/{{id}}"
        );
        assert!(
            !open.contains("Route::ChatThread"),
            "Fleet Open must not navigate to a chat thread"
        );
    }

    #[test]
    fn live_sessions_nav_is_chat_history_not_sandboxes() {
        // Docs §6.3 / sidebar §5.1: Live sessions is the same route as
        // Chat history (`/{ws}/sessions`), not /sandboxes worker rows.
        let sidebar = include_str!("../../components/sidebar.rs");
        assert_eq!(
            nav_route_for_label(sidebar, CHAT_HISTORY_NAV_LABEL),
            "Sessions"
        );
        assert_eq!(
            nav_route_for_label(sidebar, LIVE_SESSIONS_NAV_LABEL),
            "Sessions",
            "docs §6.3: sidebar {LIVE_SESSIONS_NAV_LABEL:?} must open /{{ws}}/sessions \
             (chat history), not Sandboxes / worker sessions"
        );
        assert_ne!(
            nav_route_for_label(sidebar, LIVE_SESSIONS_NAV_LABEL),
            "Sandboxes"
        );
        assert_eq!(live_sessions_nav_path(WS), chat_history_path(WS));
        assert_eq!(LIVE_SESSIONS_NAV_LABEL, "Live sessions");
        assert_eq!(CHAT_HISTORY_NAV_LABEL, "Chat history");
    }

    #[test]
    fn dioxus_routes_keep_live_view_off_the_list() {
        let main = include_str!("../../main.rs");
        let detail = main
            .find("#[route(\"/:workspace_id/sessions/:session_id\")]")
            .expect("SessionDetail route");
        let list = main
            .find("#[route(\"/:workspace_id/sessions\")]")
            .expect("Sessions list route");
        assert!(
            detail < list,
            "/sessions/{{id}} must be declared before /sessions so the list \
             does not swallow the worker live view"
        );
        assert!(main.contains("#[route(\"/:workspace_id/chat/:conversation_id\")]"));
        assert!(
            !main.contains("#[route(\"/:workspace_id/sessions/:session_id\")]\n        Sessions"),
            "Sessions list must not bind /sessions/{{id}}"
        );
    }
}
