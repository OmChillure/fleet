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

    // catalog:unit-route-nav-redirects — accordion child labels/paths.
    #[test]
    fn nav_children_labels_and_paths() {
        let children: Vec<_> = RUNNER_NAV_CHILDREN
            .iter()
            .map(|c| (c.label, c.slug, runner_child_path("ws1", c.slug)))
            .collect();
        assert_eq!(
            children,
            [
                (
                    "Self-hosted",
                    "self-hosted",
                    "/ws1/runners/self-hosted".into()
                ),
                ("Cloud", "cloud", "/ws1/runners/cloud".into()),
                ("Partners", "partners", "/ws1/runners/partners".into()),
            ]
        );
    }

    // catalog:unit-route-nav-redirects — /{ws}/workers → Self-hosted.
    #[test]
    fn workers_redirects_to_self_hosted() {
        assert_eq!(workers_redirect("acme"), "/acme/runners/self-hosted");
    }
}
