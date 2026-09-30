//! Sidebar expand state that must outlive AppShell.
//!
//! The shell remounts on every route change. Local `use_signal` defaults
//! would reopen Chat/Monitoring/Inbox (and forget collapses) on Runners.

use std::collections::HashSet;

use dioxus::prelude::{Signal, WritableExt};

use crate::Route;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NavSection {
    Chat,
    Monitoring,
    Agents,
    Runners,
    Workspace,
    Instance,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavChrome {
    pub expanded: HashSet<NavSection>,
    pub inbox_nest: bool,
    pub sidebar_open: bool,
}

impl Default for NavChrome {
    fn default() -> Self {
        Self {
            expanded: HashSet::from([NavSection::Chat, NavSection::Monitoring]),
            inbox_nest: false,
            sidebar_open: true,
        }
    }
}

pub fn is_inbox_route(route: &Route) -> bool {
    matches!(
        route,
        Route::Inbox { .. } | Route::InboxApp { .. } | Route::InboxConnection { .. }
    )
}

pub fn section_for_route(route: &Route) -> Option<NavSection> {
    match route {
        Route::Chat { .. }
        | Route::ChatThread { .. }
        | Route::Sessions { .. }
        | Route::SessionDetail { .. } => Some(NavSection::Chat),
        Route::Fleet { .. }
        | Route::Dashboard { .. }
        | Route::Sandboxes { .. }
        | Route::Cases { .. }
        | Route::CaseDetail { .. }
        | Route::Inbox { .. }
        | Route::InboxApp { .. }
        | Route::InboxConnection { .. } => Some(NavSection::Monitoring),
        Route::Agents { .. } => Some(NavSection::Agents),
        Route::SelfHosted { .. }
        | Route::CloudPage { .. }
        | Route::PartnersPage { .. }
        | Route::RunnersIndex { .. }
        | Route::WorkersRedirect { .. } => Some(NavSection::Runners),
        Route::Integrations { .. }
        | Route::ConnectionEdit { .. }
        | Route::Settings { .. } => Some(NavSection::Workspace),
        Route::Admin {} => Some(NavSection::Instance),
        _ => None,
    }
}

impl NavChrome {
    /// Make the current page's section visible. Does not reopen anything
    /// the user collapsed, and does not close the Inbox nest on leave.
    pub fn reveal_route(&mut self, route: &Route) {
        if let Some(section) = section_for_route(route) {
            self.expanded.insert(section);
        }
        if is_inbox_route(route) {
            self.inbox_nest = true;
        }
    }

    pub fn toggle_section(&mut self, section: NavSection) {
        if !self.expanded.remove(&section) {
            self.expanded.insert(section);
        }
    }

    pub fn toggle_inbox_nest(&mut self) {
        self.inbox_nest = !self.inbox_nest;
    }

    pub fn toggle_sidebar(&mut self) {
        self.sidebar_open = !self.sidebar_open;
    }
}

pub fn update_nav(mut nav: Signal<NavChrome>, edit: impl FnOnce(&mut NavChrome)) {
    let mut next = nav();
    let before = next.clone();
    edit(&mut next);
    if next != before {
        nav.set(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> String {
        "ws".into()
    }

    #[test]
    fn default_does_not_expand_inbox_or_runners() {
        let nav = NavChrome::default();
        assert!(nav.expanded.contains(&NavSection::Chat));
        assert!(nav.expanded.contains(&NavSection::Monitoring));
        assert!(!nav.expanded.contains(&NavSection::Runners));
        assert!(!nav.inbox_nest);
        assert!(nav.sidebar_open);
    }

    #[test]
    fn reveal_runners_does_not_reset_or_open_inbox() {
        let mut nav = NavChrome::default();
        nav.expanded.remove(&NavSection::Monitoring);
        nav.reveal_route(&Route::SelfHosted { workspace_id: ws() });
        assert!(
            !nav.expanded.contains(&NavSection::Monitoring),
            "collapsed Monitoring must stay collapsed"
        );
        assert!(nav.expanded.contains(&NavSection::Runners));
        assert!(nav.expanded.contains(&NavSection::Chat));
        assert!(!nav.inbox_nest);
    }

    #[test]
    fn reveal_inbox_opens_nest_without_closing_other_sections() {
        let mut nav = NavChrome::default();
        nav.expanded.insert(NavSection::Runners);
        nav.reveal_route(&Route::InboxApp {
            workspace_id: ws(),
            provider: "slack".into(),
        });
        assert!(nav.inbox_nest);
        assert!(nav.expanded.contains(&NavSection::Monitoring));
        assert!(nav.expanded.contains(&NavSection::Runners));
    }

    #[test]
    fn leaving_inbox_does_not_force_nest_closed() {
        let mut nav = NavChrome::default();
        nav.reveal_route(&Route::Inbox { workspace_id: ws() });
        assert!(nav.inbox_nest);
        nav.reveal_route(&Route::SelfHosted { workspace_id: ws() });
        assert!(
            nav.inbox_nest,
            "leave must not fight a nest the user already had open"
        );
    }

    #[test]
    fn toggle_section_is_symmetric() {
        let mut nav = NavChrome::default();
        nav.toggle_section(NavSection::Monitoring);
        assert!(!nav.expanded.contains(&NavSection::Monitoring));
        nav.toggle_section(NavSection::Monitoring);
        assert!(nav.expanded.contains(&NavSection::Monitoring));
    }

    #[test]
    fn runners_dashboard_are_not_inbox_routes() {
        assert!(!is_inbox_route(&Route::SelfHosted { workspace_id: ws() }));
        assert!(!is_inbox_route(&Route::RunnersIndex { workspace_id: ws() }));
        assert!(!is_inbox_route(&Route::Dashboard { workspace_id: ws() }));
        assert!(is_inbox_route(&Route::Inbox { workspace_id: ws() }));
    }
}
