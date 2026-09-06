//! Step 12 — run rollup (CP10). Pure decision function + K27 cancel policy.

use crate::domain::{BoardTaskStatus, OnTaskFailPolicy, WorkRunStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRollupInput {
    pub run_status: WorkRunStatus,
    pub cancel_requested: bool,
    pub task_statuses: Vec<BoardTaskStatus>,
    pub on_task_fail: OnTaskFailPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollupAction {
    /// Still has non-terminal tasks (incl. blocked).
    KeepActive,
    /// Transition run to terminal status.
    Terminal(WorkRunStatus),
}

/// Decide whether a run should terminate and with which status.
pub fn rollup_run(input: &RunRollupInput) -> RollupAction {
    // Only roll up active runs, or accepted with cancel (pre-materialize path handled elsewhere).
    if input.run_status.is_terminal() {
        return RollupAction::KeepActive; // already terminal; no-op at pure layer
    }

    if input.task_statuses.is_empty() {
        // No cards: cancel-before-materialize → cancelled; otherwise keep (waiting materialize).
        if input.cancel_requested {
            return RollupAction::Terminal(WorkRunStatus::Cancelled);
        }
        return RollupAction::KeepActive;
    }

    let all_terminal = input.task_statuses.iter().all(|s| s.is_terminal());
    if !all_terminal {
        return RollupAction::KeepActive;
    }

    // K27: cancel always wins once all tasks terminal.
    if input.cancel_requested {
        return RollupAction::Terminal(WorkRunStatus::Cancelled);
    }

    let any_failed = input.task_statuses.contains(&BoardTaskStatus::DoneFailed);
    let any_cancelled = input.task_statuses.contains(&BoardTaskStatus::Cancelled);
    let all_success = input
        .task_statuses
        .iter()
        .all(|s| *s == BoardTaskStatus::DoneSuccess);

    if any_failed {
        return match input.on_task_fail {
            OnTaskFailPolicy::FailRun => RollupAction::Terminal(WorkRunStatus::Failed),
            OnTaskFailPolicy::Leave => RollupAction::Terminal(WorkRunStatus::Completed),
        };
    }

    if all_success {
        return RollupAction::Terminal(WorkRunStatus::Completed);
    }

    // Mix of done_success + cancelled without cancel_requested → failed (invariant)
    if any_cancelled {
        return RollupAction::Terminal(WorkRunStatus::Failed);
    }

    RollupAction::Terminal(WorkRunStatus::Failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use BoardTaskStatus::*;

    fn input(
        statuses: Vec<BoardTaskStatus>,
        cancel: bool,
        policy: OnTaskFailPolicy,
    ) -> RunRollupInput {
        RunRollupInput {
            run_status: WorkRunStatus::Active,
            cancel_requested: cancel,
            task_statuses: statuses,
            on_task_fail: policy,
        }
    }

    #[test]
    fn keep_active_while_running() {
        let a = rollup_run(&input(
            vec![DoneSuccess, InProgress],
            false,
            OnTaskFailPolicy::FailRun,
        ));
        assert_eq!(a, RollupAction::KeepActive);
    }

    #[test]
    fn blocked_keeps_active() {
        let a = rollup_run(&input(
            vec![Blocked, DoneSuccess],
            false,
            OnTaskFailPolicy::FailRun,
        ));
        assert_eq!(a, RollupAction::KeepActive);
    }

    #[test]
    fn all_success_completed() {
        let a = rollup_run(&input(
            vec![DoneSuccess, DoneSuccess],
            false,
            OnTaskFailPolicy::FailRun,
        ));
        assert_eq!(a, RollupAction::Terminal(WorkRunStatus::Completed));
    }

    #[test]
    fn on_task_fail_leave_does_not_fail_run() {
        // UI fixtures use fail_run. leave must not roll the run to failed
        // when one task is done_failed — remaining work stays active, and a
        // fully-terminal board completes instead of failing the run.
        let cases: &[(&[BoardTaskStatus], bool, RollupAction)] = &[
            (
                &[DoneSuccess, DoneFailed, Queued],
                false,
                RollupAction::KeepActive,
            ),
            (
                &[DoneSuccess, DoneFailed, Ready],
                false,
                RollupAction::KeepActive,
            ),
            (
                &[DoneSuccess, DoneFailed, Assigned],
                false,
                RollupAction::KeepActive,
            ),
            (
                &[DoneSuccess, DoneFailed, InProgress],
                false,
                RollupAction::KeepActive,
            ),
            (
                &[DoneSuccess, DoneFailed, Blocked],
                false,
                RollupAction::KeepActive,
            ),
            (
                &[DoneSuccess, DoneFailed, RetryWait],
                false,
                RollupAction::KeepActive,
            ),
            (
                &[DoneFailed],
                false,
                RollupAction::Terminal(WorkRunStatus::Completed),
            ),
            (
                &[DoneSuccess, DoneFailed],
                false,
                RollupAction::Terminal(WorkRunStatus::Completed),
            ),
            (
                &[DoneFailed, DoneFailed],
                false,
                RollupAction::Terminal(WorkRunStatus::Completed),
            ),
            (
                &[DoneFailed, Cancelled],
                false,
                RollupAction::Terminal(WorkRunStatus::Completed),
            ),
            (
                &[DoneSuccess, DoneFailed, Cancelled],
                false,
                RollupAction::Terminal(WorkRunStatus::Completed),
            ),
            (
                &[DoneSuccess, DoneFailed],
                true,
                RollupAction::Terminal(WorkRunStatus::Cancelled),
            ),
        ];
        for &(board, cancel, expected) in cases {
            assert_eq!(
                rollup_run(&input(board.to_vec(), cancel, OnTaskFailPolicy::Leave)),
                expected,
                "leave board={board:?} cancel={cancel}"
            );
        }
    }

    #[test]
    fn leave_success_plus_cancelled_without_done_failed_is_failed() {
        let a = rollup_run(&input(
            vec![DoneSuccess, Cancelled],
            false,
            OnTaskFailPolicy::Leave,
        ));
        assert_eq!(a, RollupAction::Terminal(WorkRunStatus::Failed));
    }

    #[test]
    fn k27_cancel_beats_all_success() {
        let a = rollup_run(&input(
            vec![DoneSuccess, DoneSuccess],
            true,
            OnTaskFailPolicy::FailRun,
        ));
        assert_eq!(a, RollupAction::Terminal(WorkRunStatus::Cancelled));
    }

    #[test]
    fn cancel_before_materialize_empty_tasks() {
        let a = rollup_run(&RunRollupInput {
            run_status: WorkRunStatus::Accepted,
            cancel_requested: true,
            task_statuses: vec![],
            on_task_fail: OnTaskFailPolicy::FailRun,
        });
        assert_eq!(a, RollupAction::Terminal(WorkRunStatus::Cancelled));
    }

    #[test]
    fn cancelled_mix_without_operator_cancel_is_failed() {
        let a = rollup_run(&input(
            vec![DoneSuccess, Cancelled],
            false,
            OnTaskFailPolicy::FailRun,
        ));
        assert_eq!(a, RollupAction::Terminal(WorkRunStatus::Failed));
    }
}
