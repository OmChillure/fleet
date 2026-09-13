use super::status::BoardTaskStatus;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitReason {
    Cancel,
    Stale,
    Timeout,
    SpawnFailed,
    HitlResume,
    Clean,
    WorkerFailed,
    SpawnHang,
    Budget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryAction {
    None,
    RetryWait,
    Fail,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryDecision {
    pub action: RetryAction,
    pub failure_reason: Option<&'static str>,
}

pub const FAILURE_MAX_RETRIES_EXCEEDED: &str = "max_retries_exceeded";
pub const FAILURE_TIMEOUT: &str = "timeout";
pub const FAILURE_SPAWN_FAILED: &str = "spawn_failed";
pub const FAILURE_DEPENDENCY_FAILED: &str = "dependency_failed";
pub const FAILURE_DEPENDENCY_CANCELLED: &str = "dependency_cancelled";

/// K20: total_attempts = max_retries + 1
pub fn total_attempts_allowed(max_retries: i32) -> i32 {
    max_retries.max(0) + 1
}

/// Retry policy after abnormal session exit. `attempt` is post-assign (first assign → 1).
pub fn should_retry(
    attempt: i32,
    max_retries: i32,
    exit_reason: ExitReason,
    cancel_requested: bool,
    task_status: BoardTaskStatus,
) -> RetryDecision {
    if task_status.is_terminal() {
        return RetryDecision {
            action: RetryAction::None,
            failure_reason: None,
        };
    }

    if cancel_requested || exit_reason == ExitReason::Cancel {
        return RetryDecision {
            action: RetryAction::Cancel,
            failure_reason: None,
        };
    }

    match exit_reason {
        ExitReason::HitlResume | ExitReason::Clean | ExitReason::WorkerFailed => {
            return RetryDecision {
                action: RetryAction::None,
                failure_reason: None,
            };
        }
        _ => {}
    }

    if attempt < total_attempts_allowed(max_retries) {
        return RetryDecision {
            action: RetryAction::RetryWait,
            failure_reason: None,
        };
    }

    let reason = match exit_reason {
        ExitReason::Timeout | ExitReason::SpawnHang => FAILURE_TIMEOUT,
        ExitReason::SpawnFailed => FAILURE_SPAWN_FAILED,
        _ => FAILURE_MAX_RETRIES_EXCEEDED,
    };
    RetryDecision {
        action: RetryAction::Fail,
        failure_reason: Some(reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_matrix() {
        let cases = [
            (
                "max_retries=0 exhausted",
                1,
                0,
                ExitReason::Stale,
                false,
                BoardTaskStatus::InProgress,
                RetryAction::Fail,
                Some(FAILURE_MAX_RETRIES_EXCEEDED),
            ),
            (
                "max_retries=2 attempt1",
                1,
                2,
                ExitReason::Stale,
                false,
                BoardTaskStatus::InProgress,
                RetryAction::RetryWait,
                None,
            ),
            (
                "max_retries=2 attempt2",
                2,
                2,
                ExitReason::Timeout,
                false,
                BoardTaskStatus::InProgress,
                RetryAction::RetryWait,
                None,
            ),
            (
                "max_retries=2 attempt3",
                3,
                2,
                ExitReason::Timeout,
                false,
                BoardTaskStatus::InProgress,
                RetryAction::Fail,
                Some(FAILURE_TIMEOUT),
            ),
            (
                "cancel never retries",
                1,
                2,
                ExitReason::Cancel,
                true,
                BoardTaskStatus::InProgress,
                RetryAction::Cancel,
                None,
            ),
            (
                "cancel_requested",
                1,
                2,
                ExitReason::Stale,
                true,
                BoardTaskStatus::InProgress,
                RetryAction::Cancel,
                None,
            ),
            (
                "terminal none",
                1,
                2,
                ExitReason::Stale,
                false,
                BoardTaskStatus::DoneSuccess,
                RetryAction::None,
                None,
            ),
            (
                "hitl none",
                1,
                2,
                ExitReason::HitlResume,
                false,
                BoardTaskStatus::Blocked,
                RetryAction::None,
                None,
            ),
            (
                "clean none",
                1,
                2,
                ExitReason::Clean,
                false,
                BoardTaskStatus::InProgress,
                RetryAction::None,
                None,
            ),
            (
                "spawn fail retry",
                1,
                1,
                ExitReason::SpawnFailed,
                false,
                BoardTaskStatus::Assigned,
                RetryAction::RetryWait,
                None,
            ),
            (
                "spawn fail exhausted",
                2,
                1,
                ExitReason::SpawnFailed,
                false,
                BoardTaskStatus::Assigned,
                RetryAction::Fail,
                Some(FAILURE_SPAWN_FAILED),
            ),
        ];

        for (name, attempt, max_r, reason, cancel, status, want_action, want_fail) in cases {
            let got = should_retry(attempt, max_r, reason, cancel, status);
            assert_eq!(got.action, want_action, "{name} action");
            assert_eq!(got.failure_reason, want_fail, "{name} failure");
        }
    }
}
