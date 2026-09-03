//! `/{ws}/runners` and `/{ws}/workers` land on Self-hosted, not a blank accordion.
//!
//! Host: `cargo test -p fleet-ui --locked -- runners_and_workers_redirect`
//! `just test` excludes fleet-ui; `fleet-runners-view` `routes.rs` is the
//! workspace-lib twin of this lock.

#[cfg(test)]
mod tests {
    use fleet_runners_view::routes::{
        NAV_LABEL_CLOUD, NAV_LABEL_PARTNERS, NAV_LABEL_SELF_HOSTED, RUNNER_NAV_CHILDREN,
        RUNNERS_INDEX_TARGET, RUNNERS_NAV_TITLE, SEGMENT_RUNNERS, SEGMENT_SELF_HOSTED,
        SEGMENT_WORKERS, WORKERS_REDIRECT_TARGET, runner_child_path, runners_index_target,
        self_hosted_path, workers_redirect,
    };

    const WS_SLUG: &str = "acme";
    const WS_UUID: &str = "11111111-2222-3333-4444-555555555555";

    fn main_src() -> &'static str {
        include_str!("../../main.rs")
    }

    fn sidebar_src() -> &'static str {
        include_str!("../../components/sidebar.rs")
    }

    fn self_hosted_page_src() -> &'static str {
        include_str!("self_hosted/page.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("self_hosted page production source")
    }

    fn component_fn<'a>(src: &'a str, name: &str) -> &'a str {
        let needle = format!("fn {name}");
        let rest = src
            .split(&needle)
            .nth(1)
            .unwrap_or_else(|| panic!("missing {name}"));
        let end = rest.find("\nfn ").unwrap_or(rest.len());
        &rest[..end]
    }

    // catalog:list-workers-and-runners-redirect-self-hosted
    // catalog:unit-route-nav-redirects — /{ws}/runners and /{ws}/workers
    // replace onto Self-hosted (h1), not a blank Runners accordion.
    #[test]
    fn runners_and_workers_redirect_to_self_hosted_not_blank_accordion() {
        assert_eq!(NAV_LABEL_SELF_HOSTED, "Self-hosted");
        assert_eq!(RUNNERS_NAV_TITLE, "Runners");
        assert_eq!(SEGMENT_RUNNERS, "runners");
        assert_eq!(SEGMENT_WORKERS, "workers");
        assert_eq!(SEGMENT_SELF_HOSTED, "self-hosted");
        assert_eq!(RUNNERS_INDEX_TARGET, SEGMENT_SELF_HOSTED);
        assert_eq!(RUNNERS_INDEX_TARGET, "self-hosted");
        assert_eq!(WORKERS_REDIRECT_TARGET, "runners/self-hosted");
        assert_eq!(
            WORKERS_REDIRECT_TARGET,
            format!("{SEGMENT_RUNNERS}/{SEGMENT_SELF_HOSTED}")
        );

        for ws in [WS_SLUG, WS_UUID] {
            let dest = format!("/{ws}/runners/self-hosted");
            assert_eq!(self_hosted_path(ws), dest);
            assert_eq!(runners_index_target(ws), dest);
            assert_eq!(workers_redirect(ws), dest);
            assert_eq!(runners_index_target(ws), self_hosted_path(ws));
            assert_eq!(workers_redirect(ws), self_hosted_path(ws));
            assert_eq!(
                runners_index_target(ws),
                runner_child_path(ws, SEGMENT_SELF_HOSTED)
            );
            assert_ne!(runners_index_target(ws), format!("/{ws}/{SEGMENT_RUNNERS}"));
            assert_ne!(runners_index_target(ws), format!("/{ws}/{SEGMENT_WORKERS}"));
            assert_ne!(workers_redirect(ws), format!("/{ws}/{SEGMENT_WORKERS}"));
            assert_ne!(workers_redirect(ws), format!("/{ws}/{SEGMENT_RUNNERS}"));
        }

        let main = main_src();
        let self_hosted_at = main
            .find("#[route(\"/:workspace_id/runners/self-hosted\")]")
            .expect("/{ws}/runners/self-hosted must be the live Self-hosted page");
        let runners_at = main
            .find("#[route(\"/:workspace_id/runners\")]")
            .expect("/{ws}/runners must be routed so it can redirect");
        let workers_at = main
            .find("#[route(\"/:workspace_id/workers\")]")
            .expect("/{ws}/workers must be routed so it can redirect");
        assert!(
            self_hosted_at < runners_at,
            "/runners/self-hosted must be declared before /runners so the index \
             cannot swallow the Self-hosted page"
        );
        assert!(
            runners_at < workers_at,
            "/runners index and /workers alias must stay distinct redirect routes"
        );
        assert!(
            main.contains("SelfHosted { workspace_id: String }"),
            "Self-hosted must be a typed Dioxus route"
        );
        assert!(
            main.contains("RunnersIndex { workspace_id: String }"),
            "/{{ws}}/runners must bind RunnersIndex"
        );
        assert!(
            main.contains("WorkersRedirect { workspace_id: String }"),
            "/{{ws}}/workers must bind WorkersRedirect"
        );

        let mapper = component_fn(main, "route_for_runners_path");
        assert!(
            mapper.contains("if path == self_hosted_path(&workspace_id)"),
            "route_for_runners_path must recognize the Self-hosted dest"
        );
        assert!(
            mapper.contains("Route::SelfHosted { workspace_id }"),
            "Self-hosted dest must map to Route::SelfHosted, not a blank index"
        );
        let fallback = mapper
            .split("} else {")
            .nth(1)
            .expect("route_for_runners_path fallback");
        assert!(
            fallback.contains("Route::SelfHosted { workspace_id }"),
            "unknown dest must still land on Self-hosted, not a blank accordion"
        );
        assert!(
            !fallback.contains("Route::RunnersIndex")
                && !fallback.contains("Route::WorkersRedirect")
                && !fallback.contains("Route::CloudPage")
                && !fallback.contains("Route::PartnersPage"),
            "fallback must not stay on the alias or a placeholder child"
        );

        for (name, dest_call) in [
            ("RunnersIndex", "runners_index_target(&workspace_id)"),
            ("WorkersRedirect", "workers_redirect(&workspace_id)"),
        ] {
            let body = component_fn(main, name);
            assert!(
                body.contains(&format!("let dest = {dest_call}"))
                    || body.contains(dest_call),
                "{name} must use {dest_call}, not a local path"
            );
            assert!(
                body.contains("navigator.replace(route.clone())")
                    || body.contains(
                        "navigator.replace(route_for_runners_path(workspace_id.clone(), &dest))"
                    ),
                "{name} must replace onto the typed dest (Self-hosted)"
            );
            assert!(
                !body.contains("navigator.push"),
                "{name} must replace, not push, so back does not land on a blank alias"
            );
            assert!(
                body.contains("rsx! { SelfHosted { workspace_id } }")
                    || body.contains("SelfHosted { workspace_id }"),
                "{name} must paint Self-hosted immediately (blank main misses e2e h1)"
            );
            assert!(
                !body.contains("NAV_LABEL_"),
                "{name} is a redirect, not a heading of its own"
            );
        }

        let page = self_hosted_page_src();
        assert!(
            page.contains("use fleet_runners_view::routes::NAV_LABEL_SELF_HOSTED"),
            "Self-hosted h1 must be the nav label"
        );
        let h1 = page
            .split("h1 {")
            .nth(1)
            .expect("Self-hosted page must render an h1");
        assert!(
            h1.contains("\"{NAV_LABEL_SELF_HOSTED}\""),
            "h1 must render NAV_LABEL_SELF_HOSTED, not Workers or a blank accordion"
        );
        assert!(
            !h1.contains("\"Workers\""),
            "Self-hosted heading must not regress to Workers"
        );
        assert_eq!(NAV_LABEL_SELF_HOSTED, "Self-hosted");

        let sidebar = sidebar_src();
        assert!(
            sidebar.contains("title: RUNNERS_NAV_TITLE"),
            "sidebar must use RUNNERS_NAV_TITLE for the accordion"
        );
        assert!(
            sidebar.contains("for child in RUNNER_NAV_CHILDREN"),
            "Runners accordion must render RUNNER_NAV_CHILDREN, not stay blank"
        );
        assert!(
            sidebar.contains("label: child.label"),
            "accordion children must show their labels (Self-hosted / Cloud / Partners)"
        );
        assert!(
            sidebar.contains("SEGMENT_SELF_HOSTED => Route::SelfHosted"),
            "Self-hosted accordion child must open Route::SelfHosted"
        );
        assert!(
            RUNNER_NAV_CHILDREN.len() == 3,
            "Runners accordion must have Self-hosted, Cloud, Partners — not a blank list"
        );
        assert_eq!(RUNNER_NAV_CHILDREN[0].label, NAV_LABEL_SELF_HOSTED);
        assert_eq!(RUNNER_NAV_CHILDREN[0].label, "Self-hosted");
        assert_eq!(RUNNER_NAV_CHILDREN[0].slug, SEGMENT_SELF_HOSTED);
        assert_eq!(RUNNER_NAV_CHILDREN[1].label, NAV_LABEL_CLOUD);
        assert_eq!(RUNNER_NAV_CHILDREN[2].label, NAV_LABEL_PARTNERS);
        let labels: Vec<_> = RUNNER_NAV_CHILDREN.iter().map(|c| c.label).collect();
        assert_eq!(labels, ["Self-hosted", "Cloud", "Partners"]);
        assert!(
            !labels.contains(&"Workers"),
            "Workers must not remain a sidebar child; it redirects to Self-hosted"
        );
        let expand = sidebar
            .split("use_effect(move ||")
            .nth(1)
            .and_then(|rest| rest.split("use_effect(move ||").next())
            .expect("sidebar expand-on-route effect");
        assert!(
            expand.contains("Route::RunnersIndex { .. }")
                && expand.contains("Route::WorkersRedirect { .. }")
                && expand.contains("Some(NavSection::Runners)"),
            "/runners and /workers must expand the Runners accordion, not leave it blank"
        );
        assert!(
            expand.contains("Route::SelfHosted { .. }"),
            "landing on Self-hosted must keep the Runners accordion expanded"
        );
    }
}
