//! Identifier newtypes used across the control plane.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! id_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            pub fn from_uuid(id: Uuid) -> Self {
                Self(id)
            }

            pub fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }

        impl From<Uuid> for $name {
            fn from(value: Uuid) -> Self {
                Self(value)
            }
        }

        impl From<$name> for Uuid {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}

id_newtype!(
    /// Unique id for a single bus message (idempotency / dedupe key).
    MessageId
);
id_newtype!(
    /// Orchestrator-owned durable coding-agent session.
    SessionId
);
id_newtype!(
    /// Sandbox Manager-owned compute instance.
    SandboxId
);
id_newtype!(
    /// Upstream work item (Jira issue, GitHub issue, UI task, …).
    TaskId
);
id_newtype!(
    /// Multi-session workflow, if any.
    WorkflowId
);
id_newtype!(
    /// Single agent turn within a session.
    TurnId
);
id_newtype!(
    /// Tool-call id within a turn (permission correlation).
    ToolCallId
);

/// Ties a whole causal chain (usually the originating task or session).
pub type CorrelationId = MessageId;
