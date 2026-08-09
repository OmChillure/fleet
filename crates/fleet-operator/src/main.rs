//! fleet-operator — reconcile FleetSandbox CRs into locked-down worker pods.

use anyhow::Context as _;
use fleet_operator::crd::FleetSandbox;
use fleet_operator::reconcile::{error_policy, reconcile, Context, OperatorConfig};
use futures::StreamExt;
use kube::runtime::watcher::Config as WatcherConfig;
use kube::runtime::Controller;
use kube::{Api, Client, CustomResourceExt};
use std::sync::Arc;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,kube=warn")),
        )
        .with_target(true)
        .init();

    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("print-crd") => {
            print!("{}", serde_yaml::to_string(&FleetSandbox::crd())?);
            return Ok(());
        }
        Some("-h" | "--help" | "help") => {
            eprintln!(
                "fleet-operator\n  (no args)   run controller\n  print-crd    emit FleetSandbox CRD YAML"
            );
            return Ok(());
        }
        Some(other) => anyhow::bail!("unknown arg {other:?} (try print-crd)"),
        None => {}
    }

    let client = Client::try_default()
        .await
        .context("kube client (in-cluster config or KUBECONFIG)")?;
    let cfg = OperatorConfig::from_env();
    info!(
        worker_image = %cfg.worker_image_default,
        api_url = %cfg.api_service_url,
        "operator starting"
    );

    let watch_ns = std::env::var("WATCH_NAMESPACE")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let sandboxes: Api<FleetSandbox> = if let Some(ns) = watch_ns.as_deref() {
        info!(namespace = %ns, "watching namespaced FleetSandboxes");
        Api::namespaced(client.clone(), ns)
    } else {
        info!("watching FleetSandboxes in all namespaces");
        Api::all(client.clone())
    };

    let ctx = Arc::new(Context {
        client: client.clone(),
        cfg,
    });

    Controller::new(sandboxes, WatcherConfig::default())
        .run(reconcile, error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok((obj, _action)) => {
                    info!(name = %obj.name, ns = ?obj.namespace, "reconciled");
                }
                Err(e) => warn!(error = %e, "controller error"),
            }
        })
        .await;

    Ok(())
}
