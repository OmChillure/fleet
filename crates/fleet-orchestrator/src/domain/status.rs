use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardTaskStatus {
    Queued,
    Ready,
    Assigned,
    InProgress,
    Blocked,
    RetryWait,
    DoneSuccess,
    DoneFailed,
    Cancelled,
}

impl BoardTaskStatus {
    pub fn all() -> &'static [BoardTaskStatus] {
        &[
            Self::Queued,
            Self::Ready,
            Self::Assigned,
            Self::InProgress,
            Self::Blocked,
            Self::RetryWait,
            Self::DoneSuccess,
            Self::DoneFailed,
            Self::Cancelled,
        ]
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::DoneSuccess | Self::DoneFailed | Self::Cancelled)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Ready => "ready",
            Self::Assigned => "assigned",
            Self::InProgress => "in_progress",
            Self::Blocked => "blocked",
            Self::RetryWait => "retry_wait",
            Self::DoneSuccess => "done_success",
            Self::DoneFailed => "done_failed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl std::fmt::Display for BoardTaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

static ALLOWED: LazyLock<HashMap<BoardTaskStatus, Vec<BoardTaskStatus>>> = LazyLock::new(|| {
    use BoardTaskStatus::*;
    HashMap::from([
        (Queued, vec![Ready, DoneFailed, Cancelled]),
        (Ready, vec![Assigned, Cancelled]),
        (
            Assigned,
            vec![
                InProgress,
                Blocked,
                RetryWait,
                DoneSuccess,
                DoneFailed,
                Cancelled,
            ],
        ),
        (
            InProgress,
            vec![Blocked, RetryWait, DoneSuccess, DoneFailed, Cancelled],
        ),
        (Blocked, vec![Ready, Cancelled]),
        (RetryWait, vec![Ready, Cancelled]),
    ])
});

pub fn can_transition(from: BoardTaskStatus, to: BoardTaskStatus) -> bool {
    if from == to {
        return false;
    }
    ALLOWED.get(&from).map(|v| v.contains(&to)).unwrap_or(false)
}

#[derive(Debug, thiserror::Error)]
#[error("illegal board task transition: {from} → {to}")]
pub struct TransitionError {
    pub from: BoardTaskStatus,
    pub to: BoardTaskStatus,
}

pub fn transition(from: BoardTaskStatus, to: BoardTaskStatus) -> Result<(), TransitionError> {
    if can_transition(from, to) {
        Ok(())
    } else {
        Err(TransitionError { from, to })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerSessionStatus {
    Spawning,
    Healthy,
    Stuck,
    Killing,
    Exited,
    FailedSpawn,
}

impl WorkerSessionStatus {
    pub fn from_db(s: &str) -> Option<Self> {
        Some(match s {
            "spawning" => Self::Spawning,
            "healthy" => Self::Healthy,
            "stuck" => Self::Stuck,
            "killing" => Self::Killing,
            "exited" => Self::Exited,
            "failed_spawn" => Self::FailedSpawn,
            _ => return None,
        })
    }

    /// K8: spawning|healthy|stuck|killing hold capacity slots.
    pub fn holds_capacity(self) -> bool {
        matches!(
            self,
            Self::Spawning | Self::Healthy | Self::Stuck | Self::Killing
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spawning => "spawning",
            Self::Healthy => "healthy",
            Self::Stuck => "stuck",
            Self::Killing => "killing",
            Self::Exited => "exited",
            Self::FailedSpawn => "failed_spawn",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkPlanStatus {
    Draft,
    Ready,
    Accepted,
    Superseded,
}

impl WorkPlanStatus {
    pub fn is_accepted(self) -> bool {
        matches!(self, Self::Accepted)
    }

    pub fn from_db(s: &str) -> Option<Self> {
        Some(match s {
            "draft" => Self::Draft,
            "ready" => Self::Ready,
            "accepted" => Self::Accepted,
            "superseded" => Self::Superseded,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkRunStatus {
    IntentReceived,
    SourcesResolved,
    Ingested,
    Planning,
    PlanReady,
    Accepted,
    Active,
    Completed,
    Failed,
    Cancelled,
}

impl WorkRunStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    pub fn from_db(s: &str) -> Option<Self> {
        Some(match s {
            "intent_received" => Self::IntentReceived,
            "sources_resolved" => Self::SourcesResolved,
            "ingested" => Self::Ingested,
            "planning" => Self::Planning,
            "plan_ready" => Self::PlanReady,
            "accepted" => Self::Accepted,
            "active" => Self::Active,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }
}

impl BoardTaskStatus {
    pub fn from_db(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => Self::Queued,
            "ready" => Self::Ready,
            "assigned" => Self::Assigned,
            "in_progress" => Self::InProgress,
            "blocked" => Self::Blocked,
            "retry_wait" => Self::RetryWait,
            "done_success" => Self::DoneSuccess,
            "done_failed" => Self::DoneFailed,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_transition_matrix() {
        let allowed: HashMap<BoardTaskStatus, Vec<BoardTaskStatus>> = HashMap::from([
            (
                BoardTaskStatus::Queued,
                vec![
                    BoardTaskStatus::Ready,
                    BoardTaskStatus::DoneFailed,
                    BoardTaskStatus::Cancelled,
                ],
            ),
            (
                BoardTaskStatus::Ready,
                vec![BoardTaskStatus::Assigned, BoardTaskStatus::Cancelled],
            ),
            (
                BoardTaskStatus::Assigned,
                vec![
                    BoardTaskStatus::InProgress,
                    BoardTaskStatus::Blocked,
                    BoardTaskStatus::RetryWait,
                    BoardTaskStatus::DoneSuccess,
                    BoardTaskStatus::DoneFailed,
                    BoardTaskStatus::Cancelled,
                ],
            ),
            (
                BoardTaskStatus::InProgress,
                vec![
                    BoardTaskStatus::Blocked,
                    BoardTaskStatus::RetryWait,
                    BoardTaskStatus::DoneSuccess,
                    BoardTaskStatus::DoneFailed,
                    BoardTaskStatus::Cancelled,
                ],
            ),
            (
                BoardTaskStatus::Blocked,
                vec![BoardTaskStatus::Ready, BoardTaskStatus::Cancelled],
            ),
            (
                BoardTaskStatus::RetryWait,
                vec![BoardTaskStatus::Ready, BoardTaskStatus::Cancelled],
            ),
            (BoardTaskStatus::DoneSuccess, vec![]),
            (BoardTaskStatus::DoneFailed, vec![]),
            (BoardTaskStatus::Cancelled, vec![]),
        ]);

        for &from in BoardTaskStatus::all() {
            let want = allowed.get(&from).cloned().unwrap_or_default();
            for &to in BoardTaskStatus::all() {
                let got = can_transition(from, to);
                let expect = want.contains(&to);
                assert_eq!(
                    got, expect,
                    "CanTransition({from} → {to}) = {got}, want {expect}"
                );
            }
        }
    }

    #[test]
    fn self_transition_not_allowed() {
        for &s in BoardTaskStatus::all() {
            assert!(!can_transition(s, s), "self {s}");
        }
    }

    #[test]
    fn is_terminal() {
        assert!(!BoardTaskStatus::Queued.is_terminal());
        assert!(!BoardTaskStatus::InProgress.is_terminal());
        assert!(BoardTaskStatus::DoneSuccess.is_terminal());
        assert!(BoardTaskStatus::DoneFailed.is_terminal());
        assert!(BoardTaskStatus::Cancelled.is_terminal());
    }

    #[test]
    fn transition_errors() {
        assert!(transition(BoardTaskStatus::Queued, BoardTaskStatus::InProgress).is_err());
        assert!(transition(BoardTaskStatus::Ready, BoardTaskStatus::Assigned).is_ok());
    }

    #[test]
    fn illegal_task_transition_rejected() {
        // Docs §15.1: persist snake_case; illegal edges are rejected (not reopened).
        // Domain helper lock (extra). Store persist is
        // store_integration::illegal_task_transition_rejected_in_store.
        fn persist(from: &str, to: &str) -> Result<(), TransitionError> {
            let from = BoardTaskStatus::from_db(from)
                .unwrap_or_else(|| panic!("unknown board status {from}"));
            let to =
                BoardTaskStatus::from_db(to).unwrap_or_else(|| panic!("unknown board status {to}"));
            transition(from, to)
        }

        let err = persist("done_success", "ready").expect_err("done_success → ready");
        assert_eq!(err.from, BoardTaskStatus::DoneSuccess);
        assert_eq!(err.to, BoardTaskStatus::Ready);
        assert_eq!(
            err.to_string(),
            "illegal board task transition: done_success → ready"
        );
        assert!(!can_transition(err.from, err.to));

        let err = persist("cancelled", "assigned").expect_err("cancelled → assigned");
        assert_eq!(err.from, BoardTaskStatus::Cancelled);
        assert_eq!(err.to, BoardTaskStatus::Assigned);
        assert_eq!(
            err.to_string(),
            "illegal board task transition: cancelled → assigned"
        );
        assert!(!can_transition(err.from, err.to));

        let illegal = [
            ("done_failed", "ready"),
            ("done_success", "cancelled"),
            ("cancelled", "done_success"),
            ("queued", "assigned"),
            ("ready", "in_progress"),
            ("assigned", "ready"),
            ("in_progress", "assigned"),
            ("done_success", "done_success"),
            ("cancelled", "cancelled"),
        ];
        for (from, to) in illegal {
            let err = persist(from, to).expect_err(&format!("{from} → {to} must be rejected"));
            assert_eq!(err.from.as_str(), from);
            assert_eq!(err.to.as_str(), to);
            assert_eq!(
                err.to_string(),
                format!("illegal board task transition: {from} → {to}")
            );
            assert!(
                !can_transition(err.from, err.to),
                "CanTransition({from} → {to}) must be false"
            );
        }

        // Legal §15.1 path still works so this is not "every persist is rejected".
        let legal = [
            ("queued", "ready"),
            ("ready", "assigned"),
            ("assigned", "in_progress"),
            ("in_progress", "done_success"),
            ("assigned", "done_failed"),
            ("assigned", "cancelled"),
            ("in_progress", "blocked"),
            ("in_progress", "retry_wait"),
            ("blocked", "ready"),
            ("retry_wait", "ready"),
            ("queued", "done_failed"),
            ("queued", "cancelled"),
        ];
        for (from, to) in legal {
            persist(from, to).unwrap_or_else(|e| panic!("{from} → {to} must be allowed: {e}"));
            let from_s = BoardTaskStatus::from_db(from).unwrap();
            let to_s = BoardTaskStatus::from_db(to).unwrap();
            assert!(
                can_transition(from_s, to_s),
                "CanTransition({from} → {to}) must be true"
            );
        }

        for from in ["done_success", "done_failed", "cancelled"] {
            for to in BoardTaskStatus::all() {
                let err = persist(from, to.as_str())
                    .expect_err(&format!("{from} → {to} must be rejected"));
                assert_eq!(err.from.as_str(), from);
                assert_eq!(err.to, *to);
                assert!(
                    !can_transition(err.from, err.to),
                    "CanTransition({from} → {to}) must be false"
                );
            }
        }
    }

    #[test]
    fn session_capacity() {
        assert!(WorkerSessionStatus::Spawning.holds_capacity());
        assert!(WorkerSessionStatus::Healthy.holds_capacity());
        assert!(WorkerSessionStatus::Stuck.holds_capacity());
        assert!(WorkerSessionStatus::Killing.holds_capacity());
        assert!(!WorkerSessionStatus::Exited.holds_capacity());
        assert!(!WorkerSessionStatus::FailedSpawn.holds_capacity());
    }

    #[test]
    fn plan_accepted() {
        assert!(WorkPlanStatus::Accepted.is_accepted());
        assert!(!WorkPlanStatus::Draft.is_accepted());
    }
}

impl BoardTaskStatus {
    pub fn as_db(self) -> &'static str {
        self.as_str()
    }
}

impl WorkPlanStatus {
    pub fn as_db(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Ready => "ready",
            Self::Accepted => "accepted",
            Self::Superseded => "superseded",
        }
    }
}

impl WorkRunStatus {
    pub fn as_db(self) -> &'static str {
        match self {
            Self::IntentReceived => "intent_received",
            Self::SourcesResolved => "sources_resolved",
            Self::Ingested => "ingested",
            Self::Planning => "planning",
            Self::PlanReady => "plan_ready",
            Self::Accepted => "accepted",
            Self::Active => "active",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}
