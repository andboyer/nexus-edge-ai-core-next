//! Engine-side [`EntityLocalPersist`] sink backed by `nexus-store`.
//!
//! Phase 5.6 · R4. The per-camera [`SightingScheduler`] calls
//! `persist.upsert` on every emit and `persist.delete` on every GC
//! sweep — both happen on the supervisor's per-frame hot path. The
//! actual SQLite write happens on a dedicated worker drained from a
//! bounded `tokio::sync::mpsc` so the hot path stays non-blocking.
//! When the queue is full we drop the oldest update (the next emit
//! will re-stamp `last_seen_at` within `emit_interval`); GC deletes
//! are similarly best-effort because the periodic sweeper will
//! reap any row whose `last_seen_at` falls past the GC horizon.
//!
//! Fail-open: if the worker panics or the channel closes, the
//! supervisor still mints and reuses `entity_local_id`s correctly
//! within its own process lifetime — the only thing we lose is
//! crash-resilient reuse across a systemd restart.

use std::sync::Arc;

use nexus_pipeline::{EntityLocalPersist, EntityLocalUpdate};
use nexus_store::{EntityLocalUpsert, Store};
use nexus_types::{CameraId, TrackId};
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// One unit of work for the persistence worker.
enum PersistOp {
    Upsert(EntityLocalUpdate),
    Delete {
        camera_id: CameraId,
        track_id: TrackId,
    },
}

pub struct StoreEntityLocalPersist {
    tx: mpsc::Sender<PersistOp>,
}

impl StoreEntityLocalPersist {
    /// Spawn the worker task and return the supervisor-side sink.
    /// `capacity` bounds the per-engine queue depth (default `256`
    /// from the engine boot site — well above the steady-state
    /// `cameras × tracks × (1/emit_interval)`).
    #[must_use]
    pub fn spawn(store: Arc<Store>, capacity: usize) -> Self {
        let (tx, rx) = mpsc::channel::<PersistOp>(capacity.max(1));
        tokio::spawn(run_worker(store, rx));
        Self { tx }
    }
}

impl EntityLocalPersist for StoreEntityLocalPersist {
    fn upsert(&self, update: EntityLocalUpdate) {
        match self.tx.try_send(PersistOp::Upsert(update)) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                debug!("entity_local_state persist queue full; dropping upsert");
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                warn!("entity_local_state persist worker gone; dropping upsert");
            }
        }
    }

    fn delete(&self, camera_id: CameraId, track_id: TrackId) {
        match self.tx.try_send(PersistOp::Delete {
            camera_id,
            track_id,
        }) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                debug!(
                    camera_id,
                    track_id, "entity_local_state persist queue full; dropping delete"
                );
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                warn!(
                    camera_id,
                    track_id, "entity_local_state persist worker gone; dropping delete"
                );
            }
        }
    }
}

async fn run_worker(store: Arc<Store>, mut rx: mpsc::Receiver<PersistOp>) {
    while let Some(op) = rx.recv().await {
        match op {
            PersistOp::Upsert(u) => {
                let payload = EntityLocalUpsert {
                    camera_id: u.camera_id,
                    track_id: u.track_id,
                    entity_local_id: u.entity_local_id,
                    started_ts: u.started_ts,
                    last_seen_at: u.last_seen_at,
                };
                if let Err(e) = store.upsert_entity_local(&payload).await {
                    warn!(
                        camera_id = payload.camera_id,
                        track_id = payload.track_id,
                        error = %e,
                        "entity_local_state upsert failed"
                    );
                }
            }
            PersistOp::Delete {
                camera_id,
                track_id,
            } => {
                if let Err(e) = store.delete_entity_local(camera_id, track_id).await {
                    debug!(
                        camera_id,
                        track_id,
                        error = %e,
                        "entity_local_state delete failed (will be reaped by periodic sweeper)"
                    );
                }
            }
        }
    }
}
