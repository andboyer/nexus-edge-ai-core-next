//! Translate [`nexus_config::InferenceConfig::ep_priority`] into the
//! list of ORT [`ExecutionProviderDispatch`] that the session builder
//! will register, in priority order.
//!
//! M5a — `ep_priority` was previously read from config and logged, but
//! the actual session was hardcoded to CPU. This module is the
//! single source of truth for the cargo-feature → EP-type mapping;
//! both `yolo` and `yolo_world` detectors call into it.
//!
//! Conventions
//! -----------
//! * Per-EP code is gated by **two** signals:
//!   1. A cargo feature (`ep-openvino`, `ep-cuda`, …) — controls
//!      whether the EP type is even compiled into the binary.
//!   2. A runtime check on `ep_priority` — controls whether ORT
//!      tries to attach it for this particular session.
//! * **CPU is always appended** at the end if not already present.
//!   The ORT shared library always ships with the CPU EP, so this
//!   fallback is total. Even if every requested accelerator EP
//!   silently fails to register (no `.so`, no device), the session
//!   will still build.
//! * **Unknown EP names** are dropped with a `warn!` log so operators
//!   can see typos in their config without the engine refusing to
//!   boot.
//! * **`"npu"` routes through OpenVINO** with the OpenVINO EP's
//!   default device-selection behaviour. NPU is not a first-class
//!   ORT EP today; the OpenVINO runtime picks the device based on
//!   the `OV_DEVICE` env var (`CPU`, `GPU`, `NPU`, `AUTO`). The T36-S
//!   config sets `OV_DEVICE=NPU` via the per-tier compose overlay.
//!
//! Logging
//! -------
//! [`selected_for_priority`] returns both the dispatch list AND the
//! human-readable names of the EPs that were actually added. Callers
//! log the names so the operator can see exactly which EPs the
//! binary chose, vs. which they asked for in config.
//!
//! Note that ORT 2.0 silently *skips* an EP that fails to attach at
//! runtime (`with_execution_providers` returns Ok even if some EPs
//! couldn't load). To see which EPs are *actually* running per
//! session, call `session.providers()` after `commit_from_file` —
//! callers do this and include the result in their own info log.

#![cfg(feature = "ort")]

use ort::execution_providers::{CPUExecutionProvider, ExecutionProviderDispatch};
use tracing::warn;

/// Returns true iff this container can plausibly drive an Intel iGPU, dGPU,
/// or NPU through the OpenVINO EP.
///
/// We detect via kernel-level device nodes:
///   * `/dev/dri/renderD12{8,9}` — i915 / xe driver render nodes (iGPU/dGPU)
///   * `/dev/accel/accel0`       — `intel_vpu` driver (NPU on Lunar Lake)
///
/// The userspace runtime libs (libonnxruntime.so + the OpenVINO provider .so
/// + libopenvino*.so + Level Zero) are bundled in the published Docker image
/// via the `onnxruntime-openvino` PyPI wheel, so device-node visibility is
/// the only check that depends on the host. When the relevant overlay
/// (`deploy/docker-compose.tNN.yml`) is in use, `/dev/dri` is bind-mounted
/// in and the container user is added to the host's `render` group — that
/// makes the device node `stat()`-able from inside the container.
///
/// Result is cached for the process lifetime (the device topology doesn't
/// change without a host reboot, and the cost of repeated `stat()`s on every
/// session create is small but non-zero).
///
/// **Override:** set `NEXUS_OPENVINO_DEVICE=force` to force-true (useful for
/// engineers iterating on the EP wiring without an Intel accelerator) or
/// `NEXUS_OPENVINO_DEVICE=skip` to force-false (useful for verifying the
/// CPU-fallback path on a box that *does* have an iGPU). Unset / any other
/// value goes through autodetection.
pub fn openvino_runtime_available() -> bool {
    if let Ok(v) = std::env::var("NEXUS_OPENVINO_DEVICE") {
        match v.trim().to_ascii_lowercase().as_str() {
            "force" | "present" | "1" | "true" => return true,
            "skip" | "absent" | "0" | "false" => return false,
            _ => {}
        }
    }
    use std::sync::OnceLock;
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let igpu = std::path::Path::new("/dev/dri/renderD128").exists()
            || std::path::Path::new("/dev/dri/renderD129").exists();
        let npu = std::path::Path::new("/dev/accel/accel0").exists();
        igpu || npu
    })
}

/// Emit a single deduplicated WARN when OpenVINO was requested but no
/// matching Intel device is reachable. Without this, the ORT C++ side
/// logs an ugly
///   `[E:onnxruntime:default, provider_bridge_ort.cc:2141] Failed to
///   load library libonnxruntime_providers_openvino.so`
/// on every session create — but in fact our images ship that .so
/// since v0.1.5 and the *real* reason inference falls back to CPU is
/// that the device node isn't present. This WARN says exactly that.
#[cfg(feature = "ep-openvino")]
fn warn_openvino_unavailable_once() {
    use std::sync::OnceLock;
    static WARNED: OnceLock<()> = OnceLock::new();
    WARNED.get_or_init(|| {
        tracing::warn!(
            "ep_priority requested 'openvino' / 'npu' but no Intel iGPU \
             (/dev/dri/renderD12x) or NPU (/dev/accel/accel0) device node \
             is reachable inside this container; OpenVINO entries are being \
             skipped and inference will run on CPU. For hardware \
             acceleration, ensure the per-tier overlay \
             (deploy/docker-compose.tNN.yml) is layered on top of the base \
             compose file — it bind-mounts /dev/dri and adds the render \
             group to the container user. Override autodetection by setting \
             NEXUS_OPENVINO_DEVICE=force in the environment."
        );
    });
}

/// Build the list of EPs to register with the ORT session, in the
/// priority order requested by `ep_priority`. Always appends CPU as
/// the final fallback if it wasn't already in the list.
///
/// Returns `(dispatchers, names)` where `names` is a human-readable
/// label for each successfully-added EP (suffixed with `"(fallback)"`
/// for the implicit CPU append).
pub fn selected_for_priority(
    ep_priority: &[String],
) -> (Vec<ExecutionProviderDispatch>, Vec<String>) {
    selected_for_priority_inner(ep_priority, openvino_runtime_available())
}

/// Pure-function core of [`selected_for_priority`]. Split out so tests can
/// drive both the "OpenVINO device present" and "OpenVINO device absent"
/// branches deterministically, regardless of the host the test runs on.
fn selected_for_priority_inner(
    ep_priority: &[String],
    #[cfg_attr(not(feature = "ep-openvino"), allow(unused_variables))]
    openvino_available: bool,
) -> (Vec<ExecutionProviderDispatch>, Vec<String>) {
    let mut dispatchers: Vec<ExecutionProviderDispatch> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut seen_cpu = false;

    for ep in ep_priority {
        let key = ep.trim().to_ascii_lowercase();
        match key.as_str() {
            "cpu" => {
                if !seen_cpu {
                    dispatchers.push(CPUExecutionProvider::default().build());
                    names.push("cpu".into());
                    seen_cpu = true;
                }
            }

            #[cfg(feature = "ep-openvino")]
            "openvino" => {
                if openvino_available {
                    use ort::execution_providers::OpenVINOExecutionProvider;
                    dispatchers.push(OpenVINOExecutionProvider::default().build());
                    names.push("openvino".into());
                } else {
                    warn_openvino_unavailable_once();
                }
            }
            #[cfg(not(feature = "ep-openvino"))]
            "openvino" => warn!(
                "ep_priority requested 'openvino' but the binary was built without \
                 --features ep-openvino; skipping"
            ),

            #[cfg(feature = "ep-cuda")]
            "cuda" => {
                use ort::execution_providers::CUDAExecutionProvider;
                dispatchers.push(CUDAExecutionProvider::default().build());
                names.push("cuda".into());
            }
            #[cfg(not(feature = "ep-cuda"))]
            "cuda" => warn!(
                "ep_priority requested 'cuda' but the binary was built without \
                 --features ep-cuda; skipping"
            ),

            #[cfg(feature = "ep-tensorrt")]
            "tensorrt" => {
                use ort::execution_providers::TensorRTExecutionProvider;
                dispatchers.push(TensorRTExecutionProvider::default().build());
                names.push("tensorrt".into());
            }
            #[cfg(not(feature = "ep-tensorrt"))]
            "tensorrt" => warn!(
                "ep_priority requested 'tensorrt' but the binary was built without \
                 --features ep-tensorrt; skipping"
            ),

            #[cfg(feature = "ep-coreml")]
            "coreml" => {
                use ort::execution_providers::CoreMLExecutionProvider;
                dispatchers.push(CoreMLExecutionProvider::default().build());
                names.push("coreml".into());
            }
            #[cfg(not(feature = "ep-coreml"))]
            "coreml" => warn!(
                "ep_priority requested 'coreml' but the binary was built without \
                 --features ep-coreml; skipping"
            ),

            #[cfg(feature = "ep-directml")]
            "directml" => {
                use ort::execution_providers::DirectMLExecutionProvider;
                dispatchers.push(DirectMLExecutionProvider::default().build());
                names.push("directml".into());
            }
            #[cfg(not(feature = "ep-directml"))]
            "directml" => warn!(
                "ep_priority requested 'directml' but the binary was built without \
                 --features ep-directml; skipping"
            ),

            // NPU routes through OpenVINO; the operator picks the
            // actual device via the `OV_DEVICE` env var (`CPU` /
            // `GPU` / `NPU` / `AUTO`) — the per-tier compose overlay
            // for T36-S sets `OV_DEVICE=NPU`.
            #[cfg(feature = "ep-openvino")]
            "npu" => {
                if openvino_available {
                    use ort::execution_providers::OpenVINOExecutionProvider;
                    dispatchers.push(OpenVINOExecutionProvider::default().build());
                    names.push("npu(via-openvino)".into());
                } else {
                    warn_openvino_unavailable_once();
                }
            }
            #[cfg(not(feature = "ep-openvino"))]
            "npu" => warn!(
                "ep_priority requested 'npu' but the binary was built without \
                 --features ep-openvino (NPU routes through OpenVINO); skipping"
            ),

            other => warn!(ep = %other, "unknown EP name in ep_priority; ignoring"),
        }
    }

    if !seen_cpu {
        dispatchers.push(CPUExecutionProvider::default().build());
        names.push("cpu(fallback)".into());
    }

    (dispatchers, names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_priority_gives_cpu_fallback() {
        let (eps, names) = selected_for_priority(&[]);
        assert_eq!(eps.len(), 1);
        assert_eq!(names, vec!["cpu(fallback)"]);
    }

    #[test]
    fn explicit_cpu_not_duplicated() {
        let (eps, names) = selected_for_priority(&["cpu".into()]);
        assert_eq!(eps.len(), 1);
        assert_eq!(names, vec!["cpu"]);
    }

    #[test]
    fn explicit_cpu_in_middle_suppresses_fallback() {
        let (_, names) = selected_for_priority(&["bogus".into(), "cpu".into(), "alsobogus".into()]);
        // bogus dropped, cpu kept, alsobogus dropped, no fallback append.
        assert_eq!(names, vec!["cpu"]);
    }

    #[test]
    fn unknown_eps_dropped_cpu_still_appended() {
        let (eps, names) = selected_for_priority(&["bogus".into(), "alsobogus".into()]);
        assert_eq!(eps.len(), 1);
        assert_eq!(names, vec!["cpu(fallback)"]);
    }

    #[test]
    fn case_insensitive_match() {
        let (_, names) = selected_for_priority(&["CPU".into()]);
        assert_eq!(names, vec!["cpu"]);
    }

    #[test]
    fn whitespace_tolerated() {
        let (_, names) = selected_for_priority(&["  cpu  ".into()]);
        assert_eq!(names, vec!["cpu"]);
    }

    #[cfg(feature = "ep-coreml")]
    #[test]
    fn coreml_appended_when_feature_on() {
        let (eps, names) = selected_for_priority(&["coreml".into()]);
        assert_eq!(eps.len(), 2);
        assert_eq!(names, vec!["coreml", "cpu(fallback)"]);
    }

    #[cfg(not(feature = "ep-coreml"))]
    #[test]
    fn coreml_dropped_when_feature_off() {
        let (eps, names) = selected_for_priority(&["coreml".into()]);
        // CoreML skipped (warn-logged), CPU appended.
        assert_eq!(eps.len(), 1);
        assert_eq!(names, vec!["cpu(fallback)"]);
    }

    #[cfg(feature = "ep-openvino")]
    #[test]
    fn openvino_then_cpu_when_feature_on() {
        // Force the device-present branch so the test is host-independent.
        let (_, names) =
            selected_for_priority_inner(&["openvino".into(), "cpu".into()], true);
        assert_eq!(names, vec!["openvino", "cpu"]);
    }

    #[cfg(feature = "ep-openvino")]
    #[test]
    fn npu_routes_through_openvino() {
        let (_, names) = selected_for_priority_inner(&["npu".into()], true);
        assert_eq!(names, vec!["npu(via-openvino)", "cpu(fallback)"]);
    }

    /// v0.1.5 regression: when no Intel iGPU/NPU device node is reachable,
    /// `openvino` entries are skipped silently (with a one-shot WARN) and
    /// CPU still gets appended. Stops the ORT C++ side from logging
    /// `Failed to load library libonnxruntime_providers_openvino.so` on
    /// every session create on CPU-only hosts.
    #[cfg(feature = "ep-openvino")]
    #[test]
    fn openvino_dropped_when_device_absent() {
        let (eps, names) =
            selected_for_priority_inner(&["openvino".into(), "cpu".into()], false);
        // Only CPU is registered; openvino was dropped silently.
        assert_eq!(eps.len(), 1);
        assert_eq!(names, vec!["cpu"]);
    }

    #[cfg(feature = "ep-openvino")]
    #[test]
    fn npu_dropped_when_device_absent() {
        let (eps, names) = selected_for_priority_inner(&["npu".into()], false);
        // npu → openvino dropped, CPU appended as fallback.
        assert_eq!(eps.len(), 1);
        assert_eq!(names, vec!["cpu(fallback)"]);
    }

    /// The `NEXUS_OPENVINO_DEVICE=force` escape hatch lets engineers
    /// without an Intel iGPU verify the OpenVINO code path. The
    /// `=skip` variant lets engineers on Intel boxes verify the
    /// CPU-fallback path.
    #[test]
    fn env_override_force_returns_true() {
        // Use a unique-ish env-var dance so we don't fight other tests.
        // SAFETY: setting env vars in tests is process-global, but this
        // helper's reads-and-restores the prior value to keep parallel
        // tests well-behaved. The only state we mutate is one variable.
        let prev = std::env::var("NEXUS_OPENVINO_DEVICE").ok();
        std::env::set_var("NEXUS_OPENVINO_DEVICE", "force");
        let v = openvino_runtime_available();
        match prev {
            Some(p) => std::env::set_var("NEXUS_OPENVINO_DEVICE", p),
            None => std::env::remove_var("NEXUS_OPENVINO_DEVICE"),
        }
        assert!(v, "force override must return true");
    }

    #[test]
    fn env_override_skip_returns_false() {
        let prev = std::env::var("NEXUS_OPENVINO_DEVICE").ok();
        std::env::set_var("NEXUS_OPENVINO_DEVICE", "skip");
        let v = openvino_runtime_available();
        match prev {
            Some(p) => std::env::set_var("NEXUS_OPENVINO_DEVICE", p),
            None => std::env::remove_var("NEXUS_OPENVINO_DEVICE"),
        }
        assert!(!v, "skip override must return false");
    }
}
