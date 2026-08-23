-- Remove speculative tables that nothing reads or writes.
-- Do not rewrite 001–019: those checksums are already applied.
--
-- Live product tables (do not drop):
--   users, workspaces, workspace_members, workspace_invites, auth_sessions
--   integration_connections, integration_oauth_apps
--   personas, chat_conversations, chat_messages
--   ticket_monitors, ticket_inbox
--   cases, case_events
--   worker_hosts, worker_host_pair_tokens, work_claims
--   workspace_plane_runners
-- Orchestrator owns the rest (projects, runs, plans, tasks, sessions, reports).

DROP TABLE IF EXISTS audit_events;
-- Created by fleet-orchestrator init and never used. Safe if that crate
-- has not migrated this database yet.
DROP TABLE IF EXISTS project_leases;
DROP TABLE IF EXISTS idempotency_keys;
DROP TABLE IF EXISTS orchestration_events;
