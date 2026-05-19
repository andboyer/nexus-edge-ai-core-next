//! Boot-time auth posture enforcement.
//!
//! Responsibilities (added in M-Install Checkpoint 2):
//!
//! 1. **Secure-by-default token provisioning.** When `auth.mode =
//!    "dev_token"` (the new built-in default) and no `dev_token`
//!    is configured in TOML, the engine reads the persisted token
//!    from `<state_dir>/dev-token`. If that file is missing it
//!    generates a fresh 32-byte URL-safe random token, persists
//!    it with mode 0600, and prints it to the WARN log so
//!    operators can copy it into their browser exactly once.
//!
//! 2. **Non-loopback `mode = none` rejection.** Operators can
//!    still opt into "no auth" with `auth.mode = "none"`, but
//!    only when the API binds to `127.0.0.1` (or `::1`). Any
//!    other bind value with `mode = none` aborts boot — the
//!    engine refuses to leak unauthenticated writes onto a LAN.
//!
//! 3. **Grandfather WARN.** When `nexus-config`'s
//!    `load_with_compat` reports that the on-disk `nexus.toml`
//!    had no `[auth]` section, this module logs a one-time
//!    deprecation warning that names the upgrade deadline
//!    (7 days from boot). The grandfather window itself is
//!    enforced by the config crate; this module just surfaces it
//!    to the operator.

use std::fs;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use base64::engine::{general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{Duration, Utc};
use nexus_config::{AuthMode, CompatNotice, Config};

/// File on disk that holds the auto-generated dev token. Lives
/// alongside the engine state directory so a wipe of `/var/lib/nexus`
/// rotates the token along with the rest of the box's identity.
const DEV_TOKEN_FILE: &str = "dev-token";

/// Length, in raw bytes, of the generated dev token. URL-safe-no-pad
/// base64 expands this to 43 characters — long enough for a 256-bit
/// secret and short enough to copy/paste into a browser tab without
/// truncation.
const DEV_TOKEN_BYTES: usize = 32;

/// Apply Checkpoint-2 auth-posture rules. Mutates `cfg.auth.dev_token`
/// in place when a dev token is auto-generated, so the rest of the
/// engine sees the resolved value.
///
/// `state_dir` is `cfg.runtime.state_dir` resolved by the caller —
/// passed in (instead of re-derived here) so the test path can
/// point at a tempdir.
///
/// Returns `Err` for boot-fatal posture violations (currently only
/// the non-loopback `mode = none` case).
pub fn apply(cfg: &mut Config, state_dir: &Path, notice: CompatNotice) -> Result<()> {
    if notice.auth_grandfathered {
        let deadline = Utc::now() + Duration::days(7);
        eprintln!(
            "nexus-engine: WARN nexus.toml has no [auth] section; \
             pinning auth.mode = \"none\" for backward compatibility. \
             This grandfather will be removed on or after {}. Add an \
             explicit [auth] block to silence this warning — see \
             config/nexus.example.toml.",
            deadline.format("%Y-%m-%d")
        );
    }

    match cfg.auth.mode {
        AuthMode::None => enforce_loopback_only(&cfg.server.api_bind)?,
        AuthMode::DevToken => {
            if cfg.auth.dev_token.is_none() {
                let token = ensure_dev_token(state_dir)?;
                cfg.auth.dev_token = Some(token);
            } else {
                eprintln!(
                    "nexus-engine: auth: dev_token sourced from nexus.toml \
                     (auto-provisioning skipped)"
                );
            }
        }
        AuthMode::Oidc | AuthMode::Local | AuthMode::Hybrid => {
            // No-op here. The OIDC verifier is built later from
            // cfg.auth.oidc; if that block is missing, the
            // verifier itself surfaces a clear error.
        }
    }
    Ok(())
}

/// True iff `bind` resolves (textually OR via std::net parsing) to
/// a loopback address. We accept three shapes:
///
/// * `"127.0.0.1:8089"` — the canonical LAN-only bind.
/// * `"[::1]:8089"`     — IPv6 loopback.
/// * `"localhost:8089"` — string-only check; we don't DNS-resolve
///   here because boot must not block on resolver state.
fn enforce_loopback_only(bind: &str) -> Result<()> {
    // Fast path: textual match on the exact strings INSTALL.md
    // recommends. Avoids parsing pitfalls on hosts where
    // SocketAddr's lexer is stricter than the operator expected.
    let lower = bind.to_ascii_lowercase();
    if lower.starts_with("127.0.0.1:")
        || lower.starts_with("[::1]:")
        || lower.starts_with("localhost:")
    {
        return Ok(());
    }

    // Slow path: try to parse and check the IP. Anything that
    // doesn't parse + isn't textually loopback is rejected.
    if let Ok(addr) = bind.parse::<SocketAddr>() {
        if addr.ip().is_loopback() {
            return Ok(());
        }
    }

    Err(anyhow!(
        "auth.mode = \"none\" is only allowed when server.api_bind is on \
         loopback (127.0.0.1, [::1], or localhost). Got `{bind}`. Either \
         change the bind to loopback or set auth.mode = \"dev_token\" / \
         \"oidc\". See INSTALL.md §11 for details."
    ))
}

/// Read `<state_dir>/dev-token` if present; otherwise generate a
/// fresh 32-byte URL-safe random token, write it with mode 0600,
/// and log the value at WARN so operators can copy it once.
fn ensure_dev_token(state_dir: &Path) -> Result<String> {
    let path = state_dir.join(DEV_TOKEN_FILE);

    if path.exists() {
        let s = fs::read_to_string(&path)
            .with_context(|| format!("reading dev token from {}", path.display()))?
            .trim()
            .to_string();
        if s.is_empty() {
            return Err(anyhow!(
                "dev token file {} exists but is empty; delete it to regenerate",
                path.display()
            ));
        }
        eprintln!(
            "nexus-engine: auth: dev_token loaded from disk path={}",
            path.display()
        );
        return Ok(s);
    }

    fs::create_dir_all(state_dir)
        .with_context(|| format!("creating state dir {}", state_dir.display()))?;
    let token = generate_token();
    write_secret(&path, token.as_bytes())
        .with_context(|| format!("writing dev token to {}", path.display()))?;

    // The whole point of WARN here is operator visibility — INFO
    // gets filtered out on noisy boxes. We deliberately print the
    // token in plaintext: it lives in a 0600 file the operator
    // can read anyway, and surfacing it once at boot beats forcing
    // a `cat /var/lib/nexus/dev-token` round-trip on first use.
    eprintln!(
        "nexus-engine: WARN auth: generated new dev token. \
         Send `Authorization: Bearer <dev_token>` on every API call. \
         path={} dev_token={} \
         (file mode 0600; delete to rotate)",
        path.display(),
        token
    );
    Ok(token)
}

/// 32 bytes from the OS RNG, encoded as URL-safe-no-pad base64.
/// `getrandom` is the same crate `rand` ultimately calls; using it
/// directly keeps nexus-engine's dep graph one fewer crate wide.
fn generate_token() -> String {
    let mut buf = [0u8; DEV_TOKEN_BYTES];
    // OsRng.fill_bytes never fails on the platforms the engine
    // ships to (Linux, macOS); a fallback path here would be dead
    // code. Panicking is acceptable: a system without a working
    // CSPRNG cannot host a security-bearing engine in any case.
    getrandom::fill(&mut buf).expect("OS RNG must succeed for token generation");
    URL_SAFE_NO_PAD.encode(buf)
}

/// Write `bytes` to `path` with mode 0600. On non-unix systems
/// (the engine doesn't ship there, but tests can run on macOS)
/// the mode bits are skipped — `OpenOptions::mode` is unix-only.
fn write_secret(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::fs::OpenOptions;
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    f.write_all(b"\n")?;
    Ok(())
}

/// Resolve the engine state directory the same way the rest of
/// `main.rs` does. Centralised here so [`apply`] can be unit-tested
/// against a tempdir without re-implementing the lookup.
pub fn state_dir(cfg: &Config) -> PathBuf {
    cfg.runtime.state_dir.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_config::AuthConfig;

    #[test]
    fn loopback_bind_is_accepted() {
        for ok in [
            "127.0.0.1:8089",
            "127.0.0.1:1",
            "[::1]:8089",
            "localhost:8089",
            "LOCALHOST:9000",
        ] {
            enforce_loopback_only(ok).unwrap_or_else(|e| panic!("`{ok}` rejected: {e}"));
        }
    }

    #[test]
    fn non_loopback_bind_is_rejected_for_mode_none() {
        for bad in ["0.0.0.0:8089", "192.168.1.10:8089", "[::]:8089"] {
            let err = enforce_loopback_only(bad).unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("loopback") && msg.contains(bad),
                "expected loopback-rejection for `{bad}`, got: {msg}"
            );
        }
    }

    #[test]
    fn ensure_dev_token_generates_then_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let first = ensure_dev_token(dir.path()).unwrap();
        assert!(!first.is_empty(), "generated token must be non-empty");
        // 32 bytes -> 43 chars URL-safe-no-pad.
        assert_eq!(first.len(), 43, "token len = {}", first.len());

        // Second call must return the same value (no rotation on
        // boot).
        let second = ensure_dev_token(dir.path()).unwrap();
        assert_eq!(first, second);

        // File must exist with the token bytes (no trailing
        // newline once trimmed).
        let on_disk = std::fs::read_to_string(dir.path().join(DEV_TOKEN_FILE)).unwrap();
        assert_eq!(on_disk.trim(), first);

        // Mode-0600 check on unix.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::metadata(dir.path().join(DEV_TOKEN_FILE)).unwrap();
            let mode = meta.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "dev-token must be 0600, got 0o{mode:o}");
        }
    }

    #[test]
    fn apply_dev_token_branch_populates_field() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config {
            auth: AuthConfig {
                mode: AuthMode::DevToken,
                dev_token: None,
                ..AuthConfig::default()
            },
            ..Config::default()
        };
        apply(&mut cfg, dir.path(), CompatNotice::default()).unwrap();
        assert!(cfg.auth.dev_token.as_deref().map(str::len) == Some(43));
    }

    #[test]
    fn apply_none_branch_blocks_non_loopback() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config {
            auth: AuthConfig {
                mode: AuthMode::None,
                ..AuthConfig::default()
            },
            server: nexus_config::ServerConfig {
                api_bind: "0.0.0.0:8089".into(),
                ..nexus_config::ServerConfig::default()
            },
            ..Config::default()
        };
        let err = apply(&mut cfg, dir.path(), CompatNotice::default()).unwrap_err();
        assert!(err.to_string().contains("loopback"));
    }

    #[test]
    fn apply_none_branch_accepts_loopback() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config {
            auth: AuthConfig {
                mode: AuthMode::None,
                ..AuthConfig::default()
            },
            server: nexus_config::ServerConfig {
                api_bind: "127.0.0.1:8089".into(),
                ..nexus_config::ServerConfig::default()
            },
            ..Config::default()
        };
        apply(&mut cfg, dir.path(), CompatNotice::default()).unwrap();
        assert!(
            cfg.auth.dev_token.is_none(),
            "mode=none must NOT auto-provision"
        );
    }
}
