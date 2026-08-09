//! Agent Fleet Kubernetes operator library (CRD + reconcile helpers).

pub mod crd;
pub mod reconcile;

pub use crd::{FleetSandbox, FleetSandboxSpec, FleetSandboxStatus};
