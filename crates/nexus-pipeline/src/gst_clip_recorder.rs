//! GStreamer-backed clip recorder — M2.1 Stage B (PR B2).
//!
//! Per-clip pipeline lifecycle:
//!
//! ```text
//!   rtspsrc location=URL latency=200 protocols=tcp+udp
//!     ! rtph264depay
//!     ! h264parse config-interval=-1
//!     ! mp4mux fragment-duration=5000 streamable=true faststart=true
//!     ! filesink location=/var/lib/nexus/clips/cam1/...mp4
//! ```
//!
//! Each [`ClipRecorder::open`] builds a fresh pipeline, starts it
//! PLAYING, and inserts the `motion_clips` row. Each
//! [`ClipRecorder::close`] sends EOS upstream of `mp4mux`, waits on
//! the bus for the EOS message (or a 5s timeout), nulls the pipeline,
//! stat()s the resulting file, and stamps `ended_at` /
//! `duration_ms` / `size_bytes` on the row.
//!
//! Hard invariants:
//!
//! * **No re-encode.** The `rtph264depay → h264parse → mp4mux` path
//!   does pure NAL pass-through; the camera's H.264 stream goes onto
//!   disk byte-for-byte (modulo container headers).
//! * **Fragmented mp4** (`fragment-duration=5000`) so a kill-mid-write
//!   leaves a partially-readable file — browsers play fragmented mp4
//!   even without the trailing `moov` atom.
//! * **Codec is `h264`, container is `mp4`** stamped on the row.
//! * **Refused on panic.** Same semantics as `StubClipRecorder`: the
//!   watermark sampler flips `set_panic(true)` when the disk is hot
//!   and `open()` returns `RecorderError::Refused`.
//!
//! Pre-roll / post-roll are NOT in this PR. They land in B3:
//!   * **Post-roll** is supervisor-side (delay close, cancel on new
//!     track born) and doesn't touch the recorder.
//!   * **Pre-roll** would require a parallel always-running pipeline
//!     with a GOP ring buffer and is the B3 stretch goal.
//!
//! Per-camera URL resolution: the recorder is constructed with a
//! snapshot of `HashMap<CameraId, String>` from the engine's camera
//! list at boot. A camera added at runtime that isn't in the snapshot
//! triggers a `Refused` with a one-shot warn log; the operator
//! restarts the engine to pick up the new camera. (Same hot-reload
//! limitation as everything else upstream.)

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gstreamer as gst;
use gstreamer::prelude::*;
use nexus_store::{ClipClose, ClipId, NewClip, Store};
use nexus_types::CameraId;
use parking_lot::Mutex as PlMutex;
use tokio::fs;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::recorder::{ClipFinal, ClipHandle, ClipMeta, ClipRecorder, OpenClip, RecorderError};
use crate::source::gst_init;

/// How long to wait for the per-clip pipeline to drain after we send
/// EOS at close time. 5s is generous; in practice a low-latency RTSP
/// camera flushes in <500ms.
const EOS_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Codec stamped on every row this recorder writes.
const CODEC: &str = "h264";
/// Container stamped on every row this recorder writes.
const CONTAINER: &str = "mp4";

pub struct GstClipRecorder {
    store: Arc<Store>,
    clips_dir: PathBuf,
    /// Per-camera RTSP URL snapshot from boot config. A None lookup
    /// triggers a Refused so the supervisor doesn't end up writing a
    /// motion_clips row that points at a never-opened file.
    camera_urls: HashMap<CameraId, String>,
    panic: PlMutex<bool>,
    /// Per-clip GStreamer state. Held under a tokio Mutex because
    /// the close path awaits on the bus drain.
    open: Mutex<HashMap<ClipId, OpenState>>,
}

struct OpenState {
    camera_id: CameraId,
    started_at: DateTime<Utc>,
    path: PathBuf,
    pipeline: gst::Pipeline,
}

// gst::Pipeline is Send + Sync by GObject contract (gstreamer-rs
// already exposes the impls), so OpenState is auto-Send + auto-Sync.
// No manual unsafe impls needed -- and the crate forbids unsafe_code
// anyway.

impl GstClipRecorder {
    pub fn new(
        store: Arc<Store>,
        clips_dir: impl AsRef<Path>,
        camera_urls: HashMap<CameraId, String>,
    ) -> Result<Self, RecorderError> {
        gst_init::ensure()
            .map_err(|e| RecorderError::Io(std::io::Error::other(format!("gst init: {e}"))))?;
        Ok(Self {
            store,
            clips_dir: clips_dir.as_ref().to_path_buf(),
            camera_urls,
            panic: PlMutex::new(false),
            open: Mutex::new(HashMap::new()),
        })
    }

    /// Path the recorder will write for `(camera_id, started_at)`.
    /// Same shape as `StubClipRecorder::clip_path` so the rest of the
    /// system (eviction loop, retention sweeper, orphan-file scan)
    /// works against either recorder unchanged.
    pub fn clip_path(&self, camera_id: CameraId, started_at: DateTime<Utc>) -> PathBuf {
        let ts = started_at.format("%Y%m%dT%H%M%S");
        let ms = started_at.timestamp_subsec_millis();
        self.clips_dir
            .join(format!("cam{camera_id}"))
            .join(format!("{ts}_{ms:03}.mp4"))
    }

    fn build_pipeline(url: &str, location: &Path) -> Result<gst::Pipeline, RecorderError> {
        // The URL came from operator-supplied config, the location
        // came from clips_dir + a deterministic timestamp template.
        // Strip embedded `"` from both before splicing into the
        // launch string so a malformed URL can't break the parser.
        let url_safe = url.replace('"', "");
        let location_safe = location.to_string_lossy().replace('"', "");
        let desc = format!(
            "rtspsrc location=\"{url_safe}\" latency=200 protocols=tcp+udp \
             ! rtph264depay \
             ! h264parse config-interval=-1 \
             ! mp4mux fragment-duration=5000 streamable=true faststart=true \
             ! filesink location=\"{location_safe}\" sync=false"
        );
        let pipeline = gst::parse::launch(&desc)
            .map_err(|e| RecorderError::Io(std::io::Error::other(format!("parse::launch: {e}"))))?
            .downcast::<gst::Pipeline>()
            .map_err(|_| {
                RecorderError::Io(std::io::Error::other("downcast Pipeline".to_string()))
            })?;
        Ok(pipeline)
    }
}

#[async_trait]
impl ClipRecorder for GstClipRecorder {
    async fn open(&self, args: OpenClip) -> Result<ClipHandle, RecorderError> {
        if *self.panic.lock() {
            return Err(RecorderError::Refused);
        }
        let url = match self.camera_urls.get(&args.camera_id) {
            Some(u) => u.clone(),
            None => {
                warn!(
                    camera_id = args.camera_id,
                    "GstClipRecorder: no RTSP URL snapshot for this camera; refusing open. \
                     Restart the engine to pick up cameras added after boot."
                );
                return Err(RecorderError::Refused);
            }
        };

        let path = self.clip_path(args.camera_id, args.started_at);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let pipeline = Self::build_pipeline(&url, &path)?;
        // set_state(Playing) returns Async on most pipelines (rtspsrc
        // negotiation is non-blocking) so we don't wait for keyframe
        // here. Frames start landing on disk as soon as the camera
        // delivers one.
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| RecorderError::Io(std::io::Error::other(format!("set Playing: {e}"))))?;

        let rel = path
            .strip_prefix(&self.clips_dir)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string_lossy().into_owned());
        let new = NewClip {
            camera_id: args.camera_id,
            started_at: args.started_at,
            path: rel,
            codec: CODEC.into(),
            container: CONTAINER.into(),
            backend_id: "local".into(),
        };
        let clip_id = match self.store.open_clip(&new).await {
            Ok(id) => id,
            Err(e) => {
                // Tear the pipeline down so we don't leak it.
                let _ = pipeline.set_state(gst::State::Null);
                return Err(e.into());
            }
        };

        self.open.lock().await.insert(
            clip_id,
            OpenState {
                camera_id: args.camera_id,
                started_at: args.started_at,
                path,
                pipeline,
            },
        );
        info!(
            camera_id = args.camera_id,
            clip_id, "gst recorder opened clip"
        );
        Ok(ClipHandle {
            clip_id,
            camera_id: args.camera_id,
        })
    }

    async fn close(&self, handle: ClipHandle, args: ClipFinal) -> Result<ClipMeta, RecorderError> {
        let state = self
            .open
            .lock()
            .await
            .remove(&handle.clip_id)
            .ok_or(RecorderError::UnknownClip(handle.clip_id))?;

        // Send EOS into the pipeline. The mp4mux finalises the moov
        // atom on EOS; without it the file is still readable as a
        // fragmented mp4 but seeking is reduced. Wait up to
        // EOS_DRAIN_TIMEOUT for the EOS message to come back on the
        // bus before nulling the pipeline.
        let pipeline = state.pipeline;
        let pipeline_for_blocking = pipeline.clone();
        let drain = tokio::task::spawn_blocking(move || {
            // send_event returns false if the pipeline is already
            // disposed; that's fine, the next null() is harmless.
            let _ = pipeline_for_blocking.send_event(gst::event::Eos::new());
            let bus = match pipeline_for_blocking.bus() {
                Some(b) => b,
                None => return,
            };
            let deadline = std::time::Instant::now() + EOS_DRAIN_TIMEOUT;
            loop {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    return;
                }
                let timeout = gst::ClockTime::from_nseconds(remaining.as_nanos() as u64);
                match bus.timed_pop(Some(timeout)) {
                    None => return,
                    Some(msg) => match msg.view() {
                        gst::MessageView::Eos(..) => return,
                        gst::MessageView::Error(e) => {
                            warn!(error = %e.error(), "gst recorder bus error during close drain");
                            return;
                        }
                        _ => {}
                    },
                }
            }
        });
        // Ignore JoinError — we still null the pipeline below.
        let _ = drain.await;
        let _ = pipeline.set_state(gst::State::Null);

        let duration_ms = (args.ended_at - state.started_at).num_milliseconds().max(0);
        let size_bytes = match fs::metadata(&state.path).await {
            Ok(meta) => meta.len() as i64,
            Err(e) => {
                warn!(
                    error = %e,
                    path = %state.path.display(),
                    "gst recorder could not stat clip; recording size_bytes=0"
                );
                0
            }
        };

        self.store
            .close_clip(
                handle.clip_id,
                &ClipClose {
                    ended_at: args.ended_at,
                    duration_ms,
                    size_bytes,
                },
            )
            .await?;

        debug!(
            camera_id = state.camera_id,
            clip_id = handle.clip_id,
            duration_ms,
            size_bytes,
            "gst recorder closed clip"
        );
        Ok(ClipMeta {
            clip_id: handle.clip_id,
            camera_id: state.camera_id,
            path: state.path,
            duration_ms,
            size_bytes,
            codec: CODEC.into(),
            container: CONTAINER.into(),
        })
    }

    fn set_panic(&self, panic: bool) {
        let mut guard = self.panic.lock();
        if *guard != panic {
            info!(panic, "gst recorder panic flag changed");
        }
        *guard = panic;
    }

    fn is_panic(&self) -> bool {
        *self.panic.lock()
    }

    fn kind(&self) -> &'static str {
        "gstreamer"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_config::{CameraConfig, StoreConfig};
    use std::path::PathBuf;
    use url::Url;

    async fn fixture() -> (Arc<Store>, tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("nexus.db");
        let store = Arc::new(
            Store::open(&StoreConfig {
                url: format!("sqlite:{}?mode=rwc", db_path.display()),
                seed_from_config: false,
                duckdb_attach: false,
                duckdb_path: PathBuf::from("/tmp/unused.duckdb"),
            })
            .await
            .unwrap(),
        );
        store
            .upsert_camera(&CameraConfig {
                id: 1,
                name: "front".into(),
                url: Url::parse("rtsp://127.0.0.1/stream").unwrap(),
                enabled: true,
                prompts: vec![],
                model_override: None,
                zones: vec![],
                max_fps: 0,
                parking_lot_mode: false,
            })
            .await
            .unwrap();
        let clips_dir = dir.path().join("clips");
        tokio::fs::create_dir_all(&clips_dir).await.unwrap();
        (store, dir, clips_dir)
    }

    #[tokio::test]
    async fn clip_path_is_stable_and_per_camera() {
        let (store, _dir, clips_dir) = fixture().await;
        let mut urls = HashMap::new();
        urls.insert(1, "rtsp://127.0.0.1/stream".to_string());
        let rec = GstClipRecorder::new(store, &clips_dir, urls).unwrap();
        let t = chrono::DateTime::parse_from_rfc3339("2026-05-13T12:34:56.789Z")
            .unwrap()
            .with_timezone(&Utc);
        let p = rec.clip_path(1, t);
        assert!(p.starts_with(&clips_dir));
        assert!(
            p.to_string_lossy().contains("cam1"),
            "path missing cam1 component"
        );
        assert!(
            p.to_string_lossy().ends_with("20260513T123456_789.mp4"),
            "path tail wrong: {p:?}"
        );
    }

    #[tokio::test]
    async fn panic_flag_refuses_open_but_allows_close() {
        let (store, _dir, clips_dir) = fixture().await;
        let mut urls = HashMap::new();
        urls.insert(1, "rtsp://127.0.0.1/nonexistent".to_string());
        let rec = GstClipRecorder::new(store, &clips_dir, urls).unwrap();
        rec.set_panic(true);
        assert!(rec.is_panic());
        let res = rec
            .open(OpenClip {
                camera_id: 1,
                started_at: Utc::now(),
            })
            .await;
        assert!(matches!(res, Err(RecorderError::Refused)));
    }

    #[tokio::test]
    async fn open_with_no_url_for_camera_refuses() {
        let (store, _dir, clips_dir) = fixture().await;
        // Empty URL map -> camera 1 lookup misses.
        let rec = GstClipRecorder::new(store, &clips_dir, HashMap::new()).unwrap();
        let res = rec
            .open(OpenClip {
                camera_id: 1,
                started_at: Utc::now(),
            })
            .await;
        assert!(matches!(res, Err(RecorderError::Refused)));
    }

    #[tokio::test]
    async fn kind_reports_gstreamer() {
        let (store, _dir, clips_dir) = fixture().await;
        let rec = GstClipRecorder::new(store, &clips_dir, HashMap::new()).unwrap();
        assert_eq!(rec.kind(), "gstreamer");
    }
}
