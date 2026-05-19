//! M7 Phase 1 Step 5 — bus subscriber that reloads the
//! `CascadingPolicy` caches when the admin API changes the
//! `delivery_settings` or per-rule `delivery_policy_json`.
//!
//! Subscribes to:
//!   * [`topic::DELIVERY_SETTINGS_CHANGED`] →
//!     [`CascadingPolicy::reload_settings`]
//!   * [`topic::RULE_DELIVERY_POLICY_CHANGED`] →
//!     [`CascadingPolicy::reload_all_rules`]
//!
//! Bus payloads are arbitrary sentinels — the reload always re-reads
//! the store, so a Lagged subscriber that drops an intermediate
//! signal still converges as soon as the next signal arrives.

use std::sync::Arc;

use futures::StreamExt;
use nexus_bus::{topic, Bus, BusExt};
use nexus_sinks::policy::CascadingPolicy;
use nexus_store::Store;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

/// Spawn the reload task. Returns its `JoinHandle` and a oneshot
/// `Sender` the main shutdown path uses to ask the task to exit.
/// Both subscribers are best-effort: a subscribe failure logs once
/// and the task exits cleanly (manual API actions still update the
/// DB; only the *hot* reload is lost, not the data).
pub fn spawn(
    bus: Arc<dyn Bus>,
    store: Arc<Store>,
    policy: Arc<CascadingPolicy>,
) -> (JoinHandle<()>, oneshot::Sender<()>) {
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let handle = tokio::spawn(async move { run(bus, store, policy, shutdown_rx).await });
    (handle, shutdown_tx)
}

async fn run(
    bus: Arc<dyn Bus>,
    store: Arc<Store>,
    policy: Arc<CascadingPolicy>,
    shutdown: oneshot::Receiver<()>,
) {
    let mut settings_stream = match bus
        .subscribe::<serde_json::Value>(topic::DELIVERY_SETTINGS_CHANGED)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            error!(
                error = %e,
                "M7 delivery reload: failed to subscribe to delivery.settings.changed; hot reload disabled"
            );
            return;
        }
    };
    let mut rules_stream = match bus
        .subscribe::<serde_json::Value>(topic::RULE_DELIVERY_POLICY_CHANGED)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            error!(
                error = %e,
                "M7 delivery reload: failed to subscribe to rule.delivery_policy.changed; hot reload disabled"
            );
            return;
        }
    };
    info!(
        "M7 delivery reload: subscribed to delivery.settings.changed + rule.delivery_policy.changed"
    );

    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => {
                info!("M7 delivery reload: shutdown requested");
                return;
            }
            msg = settings_stream.next() => {
                match msg {
                    None => {
                        warn!("M7 delivery reload: delivery.settings.changed stream ended");
                        return;
                    }
                    Some(Err(e)) => {
                        // Lagged subscriber — we still reload below
                        // because the next signal is exactly what
                        // we need; nothing to forward, just trace it.
                        warn!(error = %e, "M7 delivery reload: settings stream error");
                    }
                    Some(Ok(_)) => {
                        if let Err(e) = policy.reload_settings(&store).await {
                            warn!(error = %e, "M7 delivery reload: reload_settings failed");
                        }
                    }
                }
            }
            msg = rules_stream.next() => {
                match msg {
                    None => {
                        warn!("M7 delivery reload: rule.delivery_policy.changed stream ended");
                        return;
                    }
                    Some(Err(e)) => {
                        warn!(error = %e, "M7 delivery reload: rules stream error");
                    }
                    Some(Ok(_)) => {
                        if let Err(e) = policy.reload_all_rules(&store).await {
                            warn!(error = %e, "M7 delivery reload: reload_all_rules failed");
                        }
                    }
                }
            }
        }
    }
}
