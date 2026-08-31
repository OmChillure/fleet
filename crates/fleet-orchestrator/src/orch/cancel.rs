//! Step 2 / PR5c — cancel drain for runs with cancel_requested_at.

use crate::sandbox::{RuntimeHandle, SandboxRuntime, Signal};
use crate::store::{BoardSnapshot, Store};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use std::sync::Arc;
use uuid::Uuid;

use super::tick::TickError;

pub struct CancelOutcome {
    pub tasks_cancelled: usize,
    pub sessions_signaled: usize,
}

/// Docs §8.3 cancel-drain action for one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CancelDrainPlan {
    pub skip: bool,
    pub mark_killing: bool,
    pub signals: Vec<Signal>,
    pub finish_cancel: bool,
}

/// TERM then KILL after `kill_grace_s`. `grace=0` may KILL immediately.
/// Must not skip TERM when `grace>0` or leave a live session healthy.
pub(crate) fn plan_cancel_live_session(
    session_status: &str,
    has_runtime_handle: bool,
    kill_grace_s: i32,
    now: DateTime<Utc>,
    kill_grace_deadline: Option<DateTime<Utc>>,
) -> CancelDrainPlan {
    if matches!(session_status, "exited" | "failed_spawn") {
        return CancelDrainPlan {
            skip: true,
            mark_killing: false,
            signals: Vec::new(),
            finish_cancel: false,
        };
    }
    if !has_runtime_handle {
        return CancelDrainPlan {
            skip: false,
            mark_killing: false,
            signals: Vec::new(),
            finish_cancel: true,
        };
    }
    if kill_grace_s <= 0 {
        // grace=0 may KILL immediately; still TERM first (harmless, not required).
        return CancelDrainPlan {
            skip: false,
            mark_killing: true,
            signals: vec![Signal::Term, Signal::Kill],
            finish_cancel: true,
        };
    }
    let grace_elapsed = kill_grace_deadline.map(|d| now >= d).unwrap_or(false);
    if grace_elapsed {
        return CancelDrainPlan {
            skip: false,
            mark_killing: true,
            signals: vec![Signal::Kill],
            finish_cancel: true,
        };
    }
    CancelDrainPlan {
        skip: false,
        mark_killing: true,
        signals: vec![Signal::Term],
        finish_cancel: false,
    }
}

fn signal_claim_name(signal: Signal) -> &'static str {
    match signal {
        Signal::Term => "term",
        Signal::Kill => "kill",
    }
}

pub async fn cancel_drain(
    store: &Store,
    runtime: &Arc<dyn SandboxRuntime>,
    project_id: Uuid,
    snap: &BoardSnapshot,
    kill_grace_s: i32,
) -> Result<CancelOutcome, TickError> {
    let runs = store.list_cancelling_runs(project_id).await?;
    let mut out = CancelOutcome {
        tasks_cancelled: 0,
        sessions_signaled: 0,
    };
    let now = Utc::now();

    for run in runs {
        // Non-terminal tasks without a live claim → cancelled immediately
        let n = store
            .cancel_nonterminal_tasks_without_session(run.id)
            .await?;
        out.tasks_cancelled += n as usize;

        // Live sessions for this run → TERM, then KILL after kill_grace_s
        for session in snap.sessions.iter().filter(|s| s.run_id == run.id) {
            let plan = plan_cancel_live_session(
                &session.status,
                session.runtime_handle.is_some(),
                kill_grace_s,
                now,
                session.kill_grace_deadline,
            );
            if plan.skip {
                continue;
            }
            if plan.mark_killing {
                let grace = now + ChronoDuration::seconds(kill_grace_s as i64);
                store
                    .mark_session_killing(session.id, "cancel", grace)
                    .await?;
            }
            if session.runtime_backend == crate::sandbox::remote::BACKEND {
                if let Some(host_id) = crate::sandbox::remote::host_id_from_handle(
                    session.runtime_handle.as_deref().unwrap_or(""),
                ) {
                    for sig in &plan.signals {
                        let _ = crate::store::enqueue_signal_session(
                            store,
                            project_id,
                            session.id,
                            host_id,
                            signal_claim_name(*sig),
                        )
                        .await;
                    }
                    if !plan.signals.is_empty() {
                        out.sessions_signaled += 1;
                    }
                }
            } else if let Some(ref hid) = session.runtime_handle {
                let h = RuntimeHandle {
                    backend: session.runtime_backend.clone(),
                    id: hid.clone(),
                };
                for sig in &plan.signals {
                    let _ = runtime.signal(&h, *sig).await;
                }
                if plan.signals.contains(&Signal::Kill) {
                    let _ = runtime.destroy(&h).await;
                }
                if plan.finish_cancel {
                    store
                        .free_session_claim(session.board_task_id, session.id, "cancel")
                        .await?;
                    store
                        .set_task_cancelled(session.board_task_id, "cancel")
                        .await?;
                    out.tasks_cancelled += 1;
                }
                if !plan.signals.is_empty() {
                    out.sessions_signaled += 1;
                }
            } else {
                // spawning without handle
                store.complete_spawn_failure(session.id).await?;
                store
                    .set_task_cancelled(session.board_task_id, "cancel")
                    .await?;
                out.tasks_cancelled += 1;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn production_src() -> &'static str {
        let src = include_str!("cancel.rs");
        match src.find("#[cfg(test)]") {
            Some(i) => &src[..i],
            None => src,
        }
    }

    fn fn_src<'a>(src: &'a str, sig: &str) -> &'a str {
        let start = src.find(sig).unwrap_or_else(|| panic!("missing {sig}"));
        let after = start + sig.len();
        let end = src[after..]
            .find("\npub async fn ")
            .or_else(|| src[after..].find("\npub(crate) fn "))
            .or_else(|| src[after..].find("\nfn "))
            .map(|i| after + i)
            .unwrap_or(src.len());
        &src[start..end]
    }

    fn assert_term_before_kill(signals: &[Signal]) {
        let term = signals.iter().position(|s| *s == Signal::Term);
        let kill = signals.iter().position(|s| *s == Signal::Kill);
        if let (Some(ti), Some(ki)) = (term, kill) {
            assert!(
                ti < ki,
                "§8.3 is TERM then KILL after kill_grace_s, got {signals:?}"
            );
        }
    }

    /// Docs §8.3: if run cancel requested, cancel tasks and TERM then KILL
    /// after `kill_grace_s`. grace=0 may KILL immediately. Must not skip
    /// TERM when grace>0 or leave sessions healthy.
    #[test]
    fn cancel_drain_signals_term_then_kill() {
        let now = Utc::now();
        let grace_s = 15;
        let deadline = now + ChronoDuration::seconds(grace_s as i64);

        let first = plan_cancel_live_session("healthy", true, grace_s, now, None);
        assert!(!first.skip, "live healthy session must be drained");
        assert!(
            first.mark_killing,
            "cancel drain must not leave the session healthy"
        );
        assert_eq!(
            first.signals,
            vec![Signal::Term],
            "grace>0 must TERM first and must not KILL before kill_grace_s"
        );
        assert!(
            first.signals.contains(&Signal::Term),
            "must not skip TERM when grace>0"
        );
        assert!(
            !first.signals.contains(&Signal::Kill),
            "must not skip the grace window when kill_grace_s>0"
        );
        assert!(
            !first.finish_cancel,
            "task/session stay live until grace elapses or the process exits"
        );
        assert_term_before_kill(&first.signals);

        let mid = plan_cancel_live_session(
            "killing",
            true,
            grace_s,
            now + ChronoDuration::seconds((grace_s - 1) as i64),
            Some(deadline),
        );
        assert!(!mid.skip);
        assert!(
            mid.mark_killing,
            "still inside grace: session must stay killing, not healthy"
        );
        assert!(
            !mid.signals.contains(&Signal::Kill),
            "KILL before kill_grace_s is not cancel drain"
        );
        assert!(
            mid.signals.contains(&Signal::Term) || mid.signals.is_empty(),
            "inside grace, re-TERM is ok; skipping TERM on the first drain is not"
        );
        assert!(
            !mid.finish_cancel,
            "inside grace the live session is not finished yet"
        );
        assert_term_before_kill(&mid.signals);

        let after = plan_cancel_live_session("killing", true, grace_s, deadline, Some(deadline));
        assert!(!after.skip);
        assert_eq!(
            after.signals,
            vec![Signal::Kill],
            "after kill_grace_s the session must be KILLed, not left running or TERM-only"
        );
        assert!(
            after.signals.contains(&Signal::Kill),
            "after kill_grace_s the session must be KILLed, not left running"
        );
        assert_ne!(
            after.signals.as_slice(),
            [Signal::Term].as_slice(),
            "after kill_grace_s, TERM-only leaves the process running"
        );
        assert!(after.finish_cancel);
        assert!(
            after.mark_killing,
            "KILL pass must not revive the session as healthy"
        );
        assert_term_before_kill(&after.signals);

        // Two ticks of cancel drain: TERM on the first, KILL only after kill_grace_s.
        let mut drain_signals = first.signals.clone();
        drain_signals.extend(after.signals.iter().copied());
        assert_eq!(
            drain_signals,
            vec![Signal::Term, Signal::Kill],
            "§8.3 cancel drain is TERM then KILL after kill_grace_s, got {drain_signals:?}"
        );
        assert_term_before_kill(&drain_signals);

        // Deadline elapsed even if the snapshot still says healthy (same-tick
        // or a missed mark) — still KILL, do not restart a new grace window.
        let late_healthy =
            plan_cancel_live_session("healthy", true, grace_s, deadline, Some(deadline));
        assert!(
            late_healthy.signals.contains(&Signal::Kill),
            "elapsed kill_grace_deadline must KILL regardless of status label, got {:?}",
            late_healthy.signals
        );
        assert!(late_healthy.finish_cancel);
        assert!(late_healthy.mark_killing);

        let zero = plan_cancel_live_session("healthy", true, 0, now, None);
        assert!(
            zero.signals.contains(&Signal::Kill),
            "grace=0 may KILL immediately"
        );
        assert!(
            zero.mark_killing,
            "grace=0 must not leave the session healthy"
        );
        assert!(zero.finish_cancel);
        assert_term_before_kill(&zero.signals);

        let neg = plan_cancel_live_session("healthy", true, -1, now, None);
        assert!(
            neg.signals.contains(&Signal::Kill),
            "negative grace is treated like 0: KILL immediately is allowed"
        );
        assert!(neg.mark_killing);
        assert!(neg.finish_cancel);
        assert_term_before_kill(&neg.signals);

        // First drain with grace>0 must not be Kill-only (must not skip TERM).
        for status in ["healthy", "spawning", "stuck", "killing"] {
            let p = plan_cancel_live_session(status, true, grace_s, now, None);
            assert!(!p.skip, "{status}: live session must be drained");
            assert!(
                p.signals.contains(&Signal::Term),
                "{status}: must not skip TERM when grace>0, got {:?}",
                p.signals
            );
            assert!(
                !p.signals.contains(&Signal::Kill),
                "{status}: grace>0 first drain must not KILL before kill_grace_s, got {:?}",
                p.signals
            );
            assert_ne!(
                p.signals.as_slice(),
                [Signal::Kill].as_slice(),
                "{status}: grace>0 Kill-only skips TERM"
            );
            assert!(p.mark_killing, "{status}: must not stay healthy");
            assert!(!p.finish_cancel, "{status}: grace window still open");
            assert_term_before_kill(&p.signals);
        }

        // Already-terminal sessions are not re-signaled.
        for status in ["exited", "failed_spawn"] {
            let p = plan_cancel_live_session(status, true, grace_s, now, None);
            assert!(p.skip, "{status}: terminal session is not a live drain");
            assert!(p.signals.is_empty(), "{status}: no TERM/KILL on a corpse");
            assert!(!p.mark_killing);
            assert!(!p.finish_cancel);
        }

        // No runtime handle: cancel the task, do not pretend to signal.
        let orphan = plan_cancel_live_session("spawning", false, grace_s, now, None);
        assert!(!orphan.skip);
        assert!(
            orphan.signals.is_empty(),
            "no handle means no TERM/KILL to deliver, got {:?}",
            orphan.signals
        );
        assert!(
            orphan.finish_cancel,
            "cancel drain must still cancel the task when there is no live handle"
        );
        assert!(
            !orphan.mark_killing,
            "a handle-less spawn is finished, not left killing"
        );

        assert_eq!(signal_claim_name(Signal::Term), "term");
        assert_eq!(signal_claim_name(Signal::Kill), "kill");

        let src = production_src();
        let drain = fn_src(src, "pub async fn cancel_drain");
        assert!(
            drain.contains("list_cancelling_runs"),
            "cancel drain only runs when a run cancel was requested:\n{drain}"
        );
        assert!(
            drain.contains("cancel_nonterminal_tasks_without_session"),
            "§8.3 cancel drain must cancel tasks, not only signal sessions:\n{drain}"
        );
        assert!(
            drain.contains("plan_cancel_live_session"),
            "cancel_drain must use the §8.3 planner, not a parallel TERM-only path:\n{drain}"
        );
        assert!(
            drain.contains("session.kill_grace_deadline"),
            "after-grace KILL must read the session deadline, not only grace==0:\n{drain}"
        );
        assert!(
            drain.contains("for sig in &plan.signals"),
            "cancel_drain must apply every planned signal (TERM then KILL):\n{drain}"
        );
        assert!(
            drain.contains("runtime.signal"),
            "local sessions must be signaled on the sandbox runtime:\n{drain}"
        );
        assert!(
            drain.contains("signal_claim_name"),
            "remote cancel drain must enqueue the planned term/kill, not a hard-coded term:\n{drain}"
        );
        assert!(
            drain.contains("if plan.mark_killing"),
            "mark_session_killing must follow the planner so a live session is not left healthy:\n{drain}"
        );
        assert!(
            drain.contains("mark_session_killing"),
            "cancel drain must mark killing so the session is not left healthy:\n{drain}"
        );
        assert!(
            drain.contains("if plan.finish_cancel"),
            "task cancel on a live handle must wait for after-grace finish, not fire on the TERM pass:\n{drain}"
        );
        assert!(
            drain.contains("set_task_cancelled"),
            "finishing a live cancel drain must cancel the claimed task:\n{drain}"
        );
        assert!(
            !drain.contains("if kill_grace_s <= 0"),
            "grace handling belongs in plan_cancel_live_session so after-grace KILL is not dropped:\n{drain}"
        );
        assert!(
            !drain.contains("runtime.signal(&h, Signal::Term)"),
            "hard-coded TERM-only skips the after-grace KILL the planner emits:\n{drain}"
        );
        assert!(
            !drain.contains("host_id, \"term\""),
            "remote cancel must enqueue the planned term/kill, not a hard-coded term:\n{drain}"
        );
        let plan_call = drain
            .find("plan_cancel_live_session(")
            .expect("cancel_drain must call the planner");
        let apply = drain
            .find("for sig in &plan.signals")
            .expect("cancel_drain must apply plan.signals");
        assert!(
            plan_call < apply,
            "signals must come from the planner, not a parallel TERM-only path:\n{drain}"
        );

        let plan_src = fn_src(src, "pub(crate) fn plan_cancel_live_session");
        assert!(
            plan_src.contains("Signal::Term"),
            "planner must be able to TERM:\n{plan_src}"
        );
        assert!(
            plan_src.contains("Signal::Kill"),
            "planner must be able to KILL after grace:\n{plan_src}"
        );
        assert!(
            plan_src.contains("grace_elapsed") || plan_src.contains("kill_grace_deadline"),
            "KILL after kill_grace_s requires the deadline, not only grace==0:\n{plan_src}"
        );
        assert!(
            plan_src.contains("kill_grace_s <= 0") || plan_src.contains("kill_grace_s == 0"),
            "grace=0 may KILL immediately — planner must treat non-positive grace:\n{plan_src}"
        );
    }
}
