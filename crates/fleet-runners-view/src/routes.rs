//! Runners route table, sidebar nav labels, and redirect helpers.

/// Sidebar accordion title (same rank as Monitoring / Agents).
pub const RUNNERS_NAV_TITLE: &str = "Runners";

/// Nav child labels — must match page headings.
pub const NAV_LABEL_SELF_HOSTED: &str = "Self-hosted";
pub const NAV_LABEL_CLOUD: &str = "Cloud";
pub const NAV_LABEL_PARTNERS: &str = "Partners";

/// Path segments under `/{workspace_id}/`.
pub const SEGMENT_RUNNERS: &str = "runners";
pub const SEGMENT_SELF_HOSTED: &str = "self-hosted";
pub const SEGMENT_CLOUD: &str = "cloud";
pub const SEGMENT_PARTNERS: &str = "partners";
pub const SEGMENT_WORKERS: &str = "workers";

/// `/{ws}/runners` lands on this child slug.
pub const RUNNERS_INDEX_TARGET: &str = SEGMENT_SELF_HOSTED;

/// `/{ws}/workers` lands on this path under `/{ws}/`.
pub const WORKERS_REDIRECT_TARGET: &str = "runners/self-hosted";

/// One child of the Runners accordion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunnerNavChild {
    pub label: &'static str,
    pub slug: &'static str,
}

/// Sidebar children in display order: Self-hosted, Cloud, Partners.
pub const RUNNER_NAV_CHILDREN: &[RunnerNavChild] = &[
    RunnerNavChild {
        label: NAV_LABEL_SELF_HOSTED,
        slug: SEGMENT_SELF_HOSTED,
    },
    RunnerNavChild {
        label: NAV_LABEL_CLOUD,
        slug: SEGMENT_CLOUD,
    },
    RunnerNavChild {
        label: NAV_LABEL_PARTNERS,
        slug: SEGMENT_PARTNERS,
    },
];

/// `/{workspace_id}/runners/{slug}`.
pub fn runner_child_path(workspace_id: &str, slug: &str) -> String {
    format!("/{workspace_id}/{SEGMENT_RUNNERS}/{slug}")
}

/// Live Self-hosted page: `/{workspace_id}/runners/self-hosted`.
pub fn self_hosted_path(workspace_id: &str) -> String {
    runner_child_path(workspace_id, SEGMENT_SELF_HOSTED)
}

/// `/{ws}/runners` → Self-hosted (`/{ws}/runners/self-hosted`).
pub fn runners_index_target(workspace_id: &str) -> String {
    runner_child_path(workspace_id, RUNNERS_INDEX_TARGET)
}

/// `/{ws}/workers` → `/{ws}/runners/self-hosted`.
pub fn workers_redirect(workspace_id: &str) -> String {
    format!("/{workspace_id}/{WORKERS_REDIRECT_TARGET}")
}

#[cfg(test)]
mod tests {
    use super::*;

    // catalog:unit-route-nav-redirects — accordion title + child labels/paths.
    #[test]
    fn nav_children_labels_and_paths() {
        assert_eq!(RUNNERS_NAV_TITLE, "Runners");
        assert_eq!(NAV_LABEL_SELF_HOSTED, "Self-hosted");
        assert_eq!(NAV_LABEL_CLOUD, "Cloud");
        assert_eq!(NAV_LABEL_PARTNERS, "Partners");
        assert_eq!(SEGMENT_RUNNERS, "runners");
        assert_eq!(RUNNER_NAV_CHILDREN.len(), 3);
        let labels: Vec<_> = RUNNER_NAV_CHILDREN.iter().map(|c| c.label).collect();
        assert_eq!(labels, ["Self-hosted", "Cloud", "Partners"]);
        assert_eq!(
            labels,
            [NAV_LABEL_SELF_HOSTED, NAV_LABEL_CLOUD, NAV_LABEL_PARTNERS]
        );
        let slugs: Vec<_> = RUNNER_NAV_CHILDREN.iter().map(|c| c.slug).collect();
        assert_eq!(slugs, ["self-hosted", "cloud", "partners"]);
        assert_eq!(
            slugs,
            [SEGMENT_SELF_HOSTED, SEGMENT_CLOUD, SEGMENT_PARTNERS]
        );
        // Live Self-hosted is first; Cloud / Partners stay placeholders.
        assert_eq!(RUNNER_NAV_CHILDREN[0].label, NAV_LABEL_SELF_HOSTED);
        assert_eq!(RUNNER_NAV_CHILDREN[0].slug, SEGMENT_SELF_HOSTED);
        assert_eq!(RUNNER_NAV_CHILDREN[0].slug, RUNNERS_INDEX_TARGET);
        for child in RUNNER_NAV_CHILDREN {
            assert_eq!(
                runner_child_path("ws1", child.slug),
                format!("/ws1/{SEGMENT_RUNNERS}/{}", child.slug)
            );
        }
        assert_eq!(
            runner_child_path("ws1", SEGMENT_SELF_HOSTED),
            "/ws1/runners/self-hosted"
        );
        assert_eq!(
            runner_child_path("ws1", SEGMENT_CLOUD),
            "/ws1/runners/cloud"
        );
        assert_eq!(
            runner_child_path("ws1", SEGMENT_PARTNERS),
            "/ws1/runners/partners"
        );
        assert_eq!(self_hosted_path("ws1"), "/ws1/runners/self-hosted");
    }

    // catalog:unit-route-nav-redirects — /{ws}/runners → Self-hosted.
    #[test]
    fn runners_index_redirects_to_self_hosted() {
        assert_eq!(RUNNERS_INDEX_TARGET, SEGMENT_SELF_HOSTED);
        assert_eq!(RUNNERS_INDEX_TARGET, "self-hosted");
        assert_eq!(runners_index_target("acme"), "/acme/runners/self-hosted");
        assert_eq!(runners_index_target("acme"), self_hosted_path("acme"));
        assert_eq!(
            runners_index_target("acme"),
            runner_child_path("acme", SEGMENT_SELF_HOSTED)
        );
        assert_ne!(
            format!("/{}/{SEGMENT_RUNNERS}", "acme"),
            runners_index_target("acme")
        );
        let ws = "11111111-2222-3333-4444-555555555555";
        assert_eq!(
            runners_index_target(ws),
            format!("/{ws}/runners/self-hosted")
        );
        assert_ne!(runners_index_target(ws), format!("/{ws}/{SEGMENT_RUNNERS}"));
        assert_ne!(runners_index_target(ws), format!("/{ws}/{SEGMENT_WORKERS}"));
    }

    // catalog:unit-route-nav-redirects — /{ws}/workers → Self-hosted.
    #[test]
    fn workers_redirects_to_self_hosted() {
        assert_eq!(SEGMENT_WORKERS, "workers");
        assert_eq!(WORKERS_REDIRECT_TARGET, "runners/self-hosted");
        assert_eq!(
            WORKERS_REDIRECT_TARGET,
            format!("{SEGMENT_RUNNERS}/{SEGMENT_SELF_HOSTED}")
        );
        assert_eq!(workers_redirect("acme"), "/acme/runners/self-hosted");
        assert_eq!(workers_redirect("acme"), self_hosted_path("acme"));
        assert_eq!(workers_redirect("acme"), runners_index_target("acme"));
        assert_ne!(
            format!("/{}/{SEGMENT_WORKERS}", "acme"),
            workers_redirect("acme")
        );
        let ws = "11111111-2222-3333-4444-555555555555";
        assert_eq!(workers_redirect(ws), format!("/{ws}/runners/self-hosted"));
        assert_eq!(workers_redirect(ws), runners_index_target(ws));
        assert_ne!(workers_redirect(ws), format!("/{ws}/{SEGMENT_WORKERS}"));
        assert_ne!(workers_redirect(ws), format!("/{ws}/{SEGMENT_RUNNERS}"));
    }
}
