//! Reconcile tick (PR5). Pure-logic control plane steps.

mod board_sync_step;
mod cancel;
mod deps_step;
mod inbox;
mod materialize_step;
mod reap;
mod review;
mod rollup;
pub(crate) mod session_context;
mod spawn;
mod stuck;
mod tick;
pub(crate) mod validate_done;

pub use board_sync_step::sync_external_board;
pub use deps_step::*;
pub use materialize_step::*;
pub use rollup::*;
pub use session_context::{list_shared_files, write_run_context};
pub use spawn::apply_retry_after_failure;
pub use tick::*;
pub use validate_done::{can_apply_task_terminal, default_acceptance, validate_task_done};

#[cfg(test)]
mod apply_task_terminal_tests {
    fn production_src(src: &str) -> &str {
        match src.find("#[cfg(test)]") {
            Some(i) => &src[..i],
            None => src,
        }
    }

    fn fn_src<'a>(src: &'a str, sig: &str) -> &'a str {
        let start = src.find(sig).unwrap_or_else(|| panic!("missing {sig}"));
        let after = start + sig.len();
        // Impl methods are indented (`    pub async fn`). A bare `\npub async fn`
        // / `\nfn ` miss would swallow every later `UPDATE board_tasks` in the
        // file and lock free_session_claim / reopen as if they were this CAS.
        let end = src[after..]
            .find("\n    pub async fn ")
            .or_else(|| src[after..].find("\n    pub fn "))
            .or_else(|| src[after..].find("\npub async fn "))
            .or_else(|| src[after..].find("\npub fn "))
            .or_else(|| src[after..].find("\n    async fn "))
            .or_else(|| src[after..].find("\nasync fn "))
            .or_else(|| src[after..].find("\npub(crate) fn "))
            .or_else(|| src[after..].find("\nfn "))
            .map(|i| after + i)
            .unwrap_or(src.len());
        &src[start..end]
    }

    fn collapse_ws(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn board_task_where_clauses(apply: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = apply;
        while let Some(u) = rest.find("UPDATE board_tasks") {
            let update = &rest[u..];
            let where_at = update
                .find("WHERE")
                .unwrap_or_else(|| panic!("UPDATE board_tasks must CAS with WHERE:\n{update}"));
            let after_where = &update[where_at + "WHERE".len()..];
            let end = after_where.find("UPDATE ").unwrap_or(after_where.len());
            out.push(collapse_ws(&after_where[..end]));
            rest = &rest[u + "UPDATE board_tasks".len()..];
        }
        out
    }

    fn status_in_lists(where_clause: &str) -> Vec<Vec<String>> {
        let mut lists = Vec::new();
        let mut from = 0;
        while let Some(i) = where_clause[from..].find("status IN (") {
            let abs = from + i + "status IN (".len();
            let close = where_clause[abs..]
                .find(')')
                .unwrap_or_else(|| panic!("unclosed status IN (:\n{where_clause}"));
            let statuses = where_clause[abs..abs + close]
                .split(',')
                .map(|s| s.trim().trim_matches('\'').to_string())
                .filter(|s| !s.is_empty())
                .collect();
            lists.push(statuses);
            from = abs + close;
        }
        lists
    }

    fn assert_fail_closed_gate(label: &str, src: &str, call: &str) -> usize {
        let mut from = 0;
        let mut last_gate_end = 0;
        let mut seen = 0usize;
        while let Some(i) = src[from..].find(call) {
            let abs = from + i;
            // can_apply_task_terminal( contains apply_task_terminal(; only count the store call.
            if call == "apply_task_terminal("
                && abs >= 4
                && src.get(abs.saturating_sub(4)..abs) == Some("can_")
            {
                from = abs + call.len();
                continue;
            }
            seen += 1;
            let before = &src[last_gate_end..abs];
            let gate_rel = before.rfind("can_apply_task_terminal(").unwrap_or_else(|| {
                panic!(
                    "{label} {call} #{seen} must consult can_apply_task_terminal so blocked HITL \
                     and a stale success after bounce cannot mutate the wrong claim"
                )
            });
            let gate_abs = last_gate_end + gate_rel;
            let head = collapse_ws(&src[gate_abs.saturating_sub(160)..abs]);
            assert!(
                !head.contains("let _ = can_apply_task_terminal")
                    && !head.contains("let _ = super::validate_done::can_apply_task_terminal")
                    && !head.contains("let _ = crate::orch::can_apply_task_terminal"),
                "{label} {call} #{seen} must not ignore can_apply_task_terminal:\n{head}"
            );
            let gated = head.contains("if !can_apply_task_terminal")
                || head.contains("if ! super::validate_done::can_apply_task_terminal")
                || head.contains("if !crate::orch::can_apply_task_terminal")
                || head.contains("if !validate_done::can_apply_task_terminal")
                || head.contains("if can_apply_task_terminal")
                || head.contains("if super::validate_done::can_apply_task_terminal")
                || head.contains("if crate::orch::can_apply_task_terminal")
                || head.contains("if validate_done::can_apply_task_terminal");
            assert!(
                gated,
                "{label} {call} #{seen} must fail-closed on can_apply_task_terminal \
                 (if !… continue/return, or if … {{ apply }}); a nearby unused call is not a gate:\n{head}"
            );
            last_gate_end = abs + call.len();
            from = last_gate_end;
        }
        seen
    }

    /// Store CAS and inbox/stuck callers must refuse blocked HITL and a stale
    /// success after bounce — not only a helper that production never consults.
    #[test]
    fn apply_task_terminal_store_and_callers_require_assigned_in_progress_claim() {
        let store = include_str!("../store/tick_ops.rs");
        let apply = fn_src(store, "pub async fn apply_task_terminal");
        assert!(
            !apply.contains("pub async fn free_session_claim")
                && !apply.contains("pub async fn set_task_retry_wait")
                && !apply.contains("pub async fn reopen_task_for_changes"),
            "fn_src must bound apply_task_terminal so later board_tasks UPDATEs are not this CAS"
        );
        let wheres = board_task_where_clauses(apply);
        assert!(
            wheres.len() >= 2,
            "apply_task_terminal must CAS both the blocked branch and done_success/done_failed"
        );
        for (i, where_clause) in wheres.iter().enumerate() {
            let lists = status_in_lists(where_clause);
            assert!(
                !lists.is_empty(),
                "board_tasks UPDATE #{i} must CAS status IN (assigned, in_progress):\n{where_clause}"
            );
            for statuses in &lists {
                let mut got = statuses.clone();
                got.sort();
                assert_eq!(
                    got,
                    ["assigned", "in_progress"],
                    "board_tasks UPDATE #{i} must CAS only from assigned/in_progress \
                     (blocked HITL must not be a from-status):\n{where_clause}"
                );
            }
            assert!(
                !where_clause.contains("'blocked'")
                    && !where_clause.contains("status = 'blocked'")
                    && !where_clause.contains("status = \"blocked\""),
                "board_tasks UPDATE #{i} WHERE must not treat blocked HITL as writable:\n{where_clause}"
            );
            // WHERE claim match — SET claimed_session_id = NULL / IS NOT NULL must not satisfy this lock.
            assert!(
                where_clause.contains("claimed_session_id = $"),
                "board_tasks UPDATE #{i} must CAS claimed_session_id = $session \
                 (not only SET NULL or IS NOT NULL):\n{where_clause}"
            );
            assert!(
                !where_clause.to_ascii_lowercase().contains(" or "),
                "board_tasks UPDATE #{i} WHERE must not OR-widen the CAS \
                 (blocked HITL / stale bounce would slip through):\n{where_clause}"
            );
        }

        let inbox = production_src(include_str!("inbox.rs"));
        let apply_fn = fn_src(inbox, "pub async fn apply_inbox");
        let apply_calls = assert_fail_closed_gate("inbox", apply_fn, "apply_task_terminal(");
        assert!(
            apply_calls > 0,
            "inbox must still call apply_task_terminal"
        );
        // Validation bounce/fail on a blocked HITL or a stale claim overwrites
        // the card even when apply_task_terminal's SQL would no-op.
        assert_fail_closed_gate("inbox", apply_fn, "reopen_task_for_changes(");
        assert_fail_closed_gate("inbox", apply_fn, "set_task_done_failed(");

        // A new orch caller must not complete the wrong claim either.
        for (label, src) in [
            ("tick", production_src(include_str!("tick.rs"))),
            ("spawn", production_src(include_str!("spawn.rs"))),
            ("reap", production_src(include_str!("reap.rs"))),
            ("cancel", production_src(include_str!("cancel.rs"))),
            ("review", production_src(include_str!("review.rs"))),
        ] {
            assert_fail_closed_gate(label, src, "apply_task_terminal(");
        }
    }
}
