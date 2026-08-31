//! Top-level bus message: envelope + typed payload.

use serde::{Deserialize, Serialize};

use crate::agent::{
    AgentEvent, AgentPermissionRequired, AgentTurnCompleted, AgentTurnFailed, AgentTurnStarted,
    ToolCallFinished, ToolCallProposed,
};
use crate::driver::{
    CancelAgentTurn, DriverHeartbeat, DriverPermissionDecision, DriverShutdownCompleted,
    InjectHumanMessage, ProvisionWorkspace, ShutdownDriver, StartAgentTurn, WorkspaceFailed,
    WorkspaceProvisioning, WorkspaceReady,
};
use crate::envelope::{Envelope, MessageScope, Producer};
use crate::integration::{
    AgentMessage, ArtifactPublished, CancelRequested, HumanMessage, IntegrationPermissionDecision,
    PermissionRequired, ReviewFeedbackReceived, SessionTerminalNotify, TaskIngested,
};
use crate::sandbox::{
    ArchiveSandbox, CreateSandbox, DestroySandbox, ExecInSandbox, SandboxArchived, SandboxCreating,
    SandboxDestroyed, SandboxFailed, SandboxReady, SandboxStopping, StopSandbox,
};
use crate::session::{
    PolicyDenied, SessionCancelled, SessionCompleted, SessionCreated, SessionFailed,
    SessionStateChanged, SessionTraceAppended,
};

/// Typed body of a bus message. Serialized as adjacent `type` + `payload`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum MessageBody {
    // --- integration inbound ---
    #[serde(rename = "integration.task_ingested")]
    IntegrationTaskIngested(TaskIngested),
    #[serde(rename = "integration.review_feedback_received")]
    IntegrationReviewFeedbackReceived(ReviewFeedbackReceived),
    #[serde(rename = "integration.human_message")]
    IntegrationHumanMessage(HumanMessage),
    #[serde(rename = "integration.cancel_requested")]
    IntegrationCancelRequested(CancelRequested),
    #[serde(rename = "integration.permission_decision")]
    IntegrationPermissionDecision(IntegrationPermissionDecision),

    // --- integration outbound ---
    #[serde(rename = "integration.agent_message")]
    IntegrationAgentMessage(AgentMessage),
    #[serde(rename = "integration.permission_required")]
    IntegrationPermissionRequired(PermissionRequired),
    #[serde(rename = "integration.session_completed")]
    IntegrationSessionCompleted(SessionTerminalNotify),
    #[serde(rename = "integration.session_failed")]
    IntegrationSessionFailed(SessionTerminalNotify),
    #[serde(rename = "integration.artifact_published")]
    IntegrationArtifactPublished(ArtifactPublished),

    // --- sandbox commands ---
    #[serde(rename = "sandbox.create")]
    SandboxCreate(CreateSandbox),
    #[serde(rename = "sandbox.stop")]
    SandboxStop(StopSandbox),
    #[serde(rename = "sandbox.archive")]
    SandboxArchive(ArchiveSandbox),
    #[serde(rename = "sandbox.destroy")]
    SandboxDestroy(DestroySandbox),
    #[serde(rename = "sandbox.exec")]
    SandboxExec(ExecInSandbox),

    // --- sandbox events ---
    #[serde(rename = "sandbox.creating")]
    SandboxCreating(SandboxCreating),
    #[serde(rename = "sandbox.ready")]
    SandboxReady(SandboxReady),
    #[serde(rename = "sandbox.unhealthy")]
    SandboxUnhealthy(SandboxFailed),
    #[serde(rename = "sandbox.failed")]
    SandboxFailed(SandboxFailed),
    #[serde(rename = "sandbox.stopping")]
    SandboxStopping(SandboxStopping),
    #[serde(rename = "sandbox.destroyed")]
    SandboxDestroyed(SandboxDestroyed),
    #[serde(rename = "sandbox.archived")]
    SandboxArchived(SandboxArchived),

    // --- driver commands ---
    #[serde(rename = "driver.provision_workspace")]
    DriverProvisionWorkspace(ProvisionWorkspace),
    #[serde(rename = "driver.start_agent_turn")]
    DriverStartAgentTurn(StartAgentTurn),
    #[serde(rename = "driver.cancel_agent_turn")]
    DriverCancelAgentTurn(CancelAgentTurn),
    #[serde(rename = "driver.permission_decision")]
    DriverPermissionDecision(DriverPermissionDecision),
    #[serde(rename = "driver.inject_human_message")]
    DriverInjectHumanMessage(InjectHumanMessage),
    #[serde(rename = "driver.shutdown")]
    DriverShutdown(ShutdownDriver),

    // --- driver / workspace events ---
    #[serde(rename = "driver.heartbeat")]
    DriverHeartbeat(DriverHeartbeat),
    #[serde(rename = "workspace.provisioning")]
    WorkspaceProvisioning(WorkspaceProvisioning),
    #[serde(rename = "workspace.ready")]
    WorkspaceReady(WorkspaceReady),
    #[serde(rename = "workspace.failed")]
    WorkspaceFailed(WorkspaceFailed),
    #[serde(rename = "driver.shutdown_completed")]
    DriverShutdownCompleted(DriverShutdownCompleted),

    // --- agent events ---
    #[serde(rename = "agent.turn_started")]
    AgentTurnStarted(AgentTurnStarted),
    #[serde(rename = "agent.event")]
    AgentEvent(AgentEvent),
    #[serde(rename = "agent.tool_call_proposed")]
    AgentToolCallProposed(ToolCallProposed),
    #[serde(rename = "agent.permission_required")]
    AgentPermissionRequired(AgentPermissionRequired),
    #[serde(rename = "agent.tool_call_finished")]
    AgentToolCallFinished(ToolCallFinished),
    #[serde(rename = "agent.turn_completed")]
    AgentTurnCompleted(AgentTurnCompleted),
    #[serde(rename = "agent.turn_failed")]
    AgentTurnFailed(AgentTurnFailed),

    // --- session ---
    #[serde(rename = "session.created")]
    SessionCreated(SessionCreated),
    #[serde(rename = "session.state_changed")]
    SessionStateChanged(SessionStateChanged),
    #[serde(rename = "session.trace_appended")]
    SessionTraceAppended(SessionTraceAppended),
    #[serde(rename = "session.completed")]
    SessionCompleted(SessionCompleted),
    #[serde(rename = "session.failed")]
    SessionFailed(SessionFailed),
    #[serde(rename = "session.cancelled")]
    SessionCancelled(SessionCancelled),
    #[serde(rename = "policy.denied")]
    PolicyDenied(PolicyDenied),

    // --- generic command ack (queryable replies) ---
    #[serde(rename = "cmd.ack")]
    CmdAck(CmdAck),
}

impl MessageBody {
    /// Wire `type` string, e.g. `sandbox.failed`.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::IntegrationTaskIngested(_) => "integration.task_ingested",
            Self::IntegrationReviewFeedbackReceived(_) => "integration.review_feedback_received",
            Self::IntegrationHumanMessage(_) => "integration.human_message",
            Self::IntegrationCancelRequested(_) => "integration.cancel_requested",
            Self::IntegrationPermissionDecision(_) => "integration.permission_decision",
            Self::IntegrationAgentMessage(_) => "integration.agent_message",
            Self::IntegrationPermissionRequired(_) => "integration.permission_required",
            Self::IntegrationSessionCompleted(_) => "integration.session_completed",
            Self::IntegrationSessionFailed(_) => "integration.session_failed",
            Self::IntegrationArtifactPublished(_) => "integration.artifact_published",
            Self::SandboxCreate(_) => "sandbox.create",
            Self::SandboxStop(_) => "sandbox.stop",
            Self::SandboxArchive(_) => "sandbox.archive",
            Self::SandboxDestroy(_) => "sandbox.destroy",
            Self::SandboxExec(_) => "sandbox.exec",
            Self::SandboxCreating(_) => "sandbox.creating",
            Self::SandboxReady(_) => "sandbox.ready",
            Self::SandboxUnhealthy(_) => "sandbox.unhealthy",
            Self::SandboxFailed(_) => "sandbox.failed",
            Self::SandboxStopping(_) => "sandbox.stopping",
            Self::SandboxDestroyed(_) => "sandbox.destroyed",
            Self::SandboxArchived(_) => "sandbox.archived",
            Self::DriverProvisionWorkspace(_) => "driver.provision_workspace",
            Self::DriverStartAgentTurn(_) => "driver.start_agent_turn",
            Self::DriverCancelAgentTurn(_) => "driver.cancel_agent_turn",
            Self::DriverPermissionDecision(_) => "driver.permission_decision",
            Self::DriverInjectHumanMessage(_) => "driver.inject_human_message",
            Self::DriverShutdown(_) => "driver.shutdown",
            Self::DriverHeartbeat(_) => "driver.heartbeat",
            Self::WorkspaceProvisioning(_) => "workspace.provisioning",
            Self::WorkspaceReady(_) => "workspace.ready",
            Self::WorkspaceFailed(_) => "workspace.failed",
            Self::DriverShutdownCompleted(_) => "driver.shutdown_completed",
            Self::AgentTurnStarted(_) => "agent.turn_started",
            Self::AgentEvent(_) => "agent.event",
            Self::AgentToolCallProposed(_) => "agent.tool_call_proposed",
            Self::AgentPermissionRequired(_) => "agent.permission_required",
            Self::AgentToolCallFinished(_) => "agent.tool_call_finished",
            Self::AgentTurnCompleted(_) => "agent.turn_completed",
            Self::AgentTurnFailed(_) => "agent.turn_failed",
            Self::SessionCreated(_) => "session.created",
            Self::SessionStateChanged(_) => "session.state_changed",
            Self::SessionTraceAppended(_) => "session.trace_appended",
            Self::SessionCompleted(_) => "session.completed",
            Self::SessionFailed(_) => "session.failed",
            Self::SessionCancelled(_) => "session.cancelled",
            Self::PolicyDenied(_) => "policy.denied",
            Self::CmdAck(_) => "cmd.ack",
        }
    }

    /// Scope identifiers present on this payload (for mirroring onto the envelope).
    pub fn scope(&self) -> MessageScope {
        match self {
            Self::IntegrationTaskIngested(p) => MessageScope {
                task_id: Some(p.task_id),
                ..Default::default()
            },
            Self::IntegrationReviewFeedbackReceived(p) => MessageScope {
                task_id: Some(p.task_id),
                session_id: p.session_id,
                ..Default::default()
            },
            Self::IntegrationHumanMessage(p) => MessageScope {
                session_id: Some(p.session_id),
                turn_id: p.turn_id,
                ..Default::default()
            },
            Self::IntegrationCancelRequested(p) => MessageScope {
                session_id: p.session_id,
                task_id: p.task_id,
                ..Default::default()
            },
            Self::IntegrationPermissionDecision(p) => MessageScope {
                session_id: Some(p.session_id),
                turn_id: Some(p.turn_id),
                ..Default::default()
            },
            Self::IntegrationAgentMessage(p) => MessageScope {
                session_id: Some(p.session_id),
                turn_id: Some(p.turn_id),
                ..Default::default()
            },
            Self::IntegrationPermissionRequired(p) => MessageScope {
                session_id: Some(p.session_id),
                turn_id: Some(p.turn_id),
                ..Default::default()
            },
            Self::IntegrationSessionCompleted(p) | Self::IntegrationSessionFailed(p) => {
                MessageScope {
                    session_id: Some(p.session_id),
                    task_id: Some(p.task_id),
                    ..Default::default()
                }
            }
            Self::IntegrationArtifactPublished(p) => MessageScope {
                session_id: Some(p.session_id),
                task_id: Some(p.task_id),
                ..Default::default()
            },
            Self::SandboxCreate(p) => MessageScope {
                sandbox_id: Some(p.sandbox_id),
                session_id: p.session_ids.first().copied(),
                ..Default::default()
            },
            Self::SandboxStop(p) => MessageScope::sandbox(p.sandbox_id),
            Self::SandboxArchive(p) => MessageScope::sandbox(p.sandbox_id),
            Self::SandboxDestroy(p) => MessageScope::sandbox(p.sandbox_id),
            Self::SandboxExec(p) => MessageScope::sandbox(p.sandbox_id),
            Self::SandboxCreating(p) => MessageScope::sandbox(p.sandbox_id),
            Self::SandboxReady(p) => MessageScope::sandbox(p.sandbox_id),
            Self::SandboxUnhealthy(p) | Self::SandboxFailed(p) => {
                MessageScope::sandbox(p.sandbox_id)
            }
            Self::SandboxStopping(p) => MessageScope::sandbox(p.sandbox_id),
            Self::SandboxDestroyed(p) => MessageScope::sandbox(p.sandbox_id),
            Self::SandboxArchived(p) => MessageScope::sandbox(p.sandbox_id),
            Self::DriverProvisionWorkspace(p) => MessageScope {
                session_id: Some(p.session_id),
                sandbox_id: Some(p.sandbox_id),
                ..Default::default()
            },
            Self::DriverStartAgentTurn(p) => MessageScope {
                session_id: Some(p.session_id),
                turn_id: Some(p.turn_id),
                ..Default::default()
            },
            Self::DriverCancelAgentTurn(p) => MessageScope {
                session_id: Some(p.session_id),
                turn_id: Some(p.turn_id),
                ..Default::default()
            },
            Self::DriverPermissionDecision(p) => MessageScope {
                session_id: Some(p.session_id),
                turn_id: Some(p.turn_id),
                ..Default::default()
            },
            Self::DriverInjectHumanMessage(p) => MessageScope {
                session_id: Some(p.session_id),
                turn_id: p.turn_id,
                ..Default::default()
            },
            Self::DriverShutdown(p) => MessageScope {
                session_id: p.session_id,
                ..Default::default()
            },
            Self::DriverHeartbeat(p) => MessageScope {
                session_id: p.session_id,
                sandbox_id: Some(p.sandbox_id),
                ..Default::default()
            },
            Self::WorkspaceProvisioning(p) => MessageScope {
                session_id: Some(p.session_id),
                sandbox_id: Some(p.sandbox_id),
                ..Default::default()
            },
            Self::WorkspaceReady(p) => MessageScope {
                session_id: Some(p.session_id),
                sandbox_id: Some(p.sandbox_id),
                ..Default::default()
            },
            Self::WorkspaceFailed(p) => MessageScope {
                session_id: Some(p.session_id),
                sandbox_id: Some(p.sandbox_id),
                ..Default::default()
            },
            Self::DriverShutdownCompleted(p) => MessageScope {
                session_id: p.session_id,
                sandbox_id: Some(p.sandbox_id),
                ..Default::default()
            },
            Self::AgentTurnStarted(p) => MessageScope {
                session_id: Some(p.session_id),
                turn_id: Some(p.turn_id),
                ..Default::default()
            },
            Self::AgentEvent(p) => MessageScope {
                session_id: Some(p.session_id),
                turn_id: Some(p.turn_id),
                ..Default::default()
            },
            Self::AgentToolCallProposed(p) => MessageScope {
                session_id: Some(p.session_id),
                turn_id: Some(p.turn_id),
                ..Default::default()
            },
            Self::AgentPermissionRequired(p) => MessageScope {
                session_id: Some(p.session_id),
                turn_id: Some(p.turn_id),
                ..Default::default()
            },
            Self::AgentToolCallFinished(p) => MessageScope {
                session_id: Some(p.session_id),
                turn_id: Some(p.turn_id),
                ..Default::default()
            },
            Self::AgentTurnCompleted(p) => MessageScope {
                session_id: Some(p.session_id),
                turn_id: Some(p.turn_id),
                ..Default::default()
            },
            Self::AgentTurnFailed(p) => MessageScope {
                session_id: Some(p.session_id),
                turn_id: Some(p.turn_id),
                ..Default::default()
            },
            Self::SessionCreated(p) => MessageScope {
                session_id: Some(p.session_id),
                task_id: p.task_id,
                ..Default::default()
            },
            Self::SessionStateChanged(p) => MessageScope::session(p.session_id),
            Self::SessionTraceAppended(p) => MessageScope::session(p.session_id),
            Self::SessionCompleted(p) => MessageScope::session(p.session_id),
            Self::SessionFailed(p) => MessageScope::session(p.session_id),
            Self::SessionCancelled(p) => MessageScope::session(p.session_id),
            Self::PolicyDenied(p) => MessageScope::session(p.session_id),
            Self::CmdAck(_) => MessageScope::default(),
        }
    }
}

/// Lightweight ack for queryable / request-reply command paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CmdAck {
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Full bus message: flattened envelope + adjacent `type`/`payload`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BusMessage {
    #[serde(flatten)]
    pub envelope: Envelope,
    #[serde(flatten)]
    pub body: MessageBody,
}

impl BusMessage {
    /// Build a message and mirror payload scope ids onto the envelope.
    ///
    /// Envelope `session_id` / `sandbox_id` / `task_id` / `turn_id` are filled
    /// from the body when present so transport key expressions and consumers
    /// that read only the envelope stay consistent with the payload.
    pub fn new(producer: Producer, body: MessageBody) -> Self {
        let mut envelope = Envelope::new(producer);
        envelope.merge_scope(body.scope());
        Self { envelope, body }
    }

    /// Build with an explicit envelope. Does **not** auto-merge scope; call
    /// [`Envelope::merge_scope`] yourself if desired.
    pub fn with_envelope(envelope: Envelope, body: MessageBody) -> Self {
        Self { envelope, body }
    }

    pub fn type_name(&self) -> &'static str {
        self.body.type_name()
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn to_json_vec(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    pub fn from_json_slice(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}
