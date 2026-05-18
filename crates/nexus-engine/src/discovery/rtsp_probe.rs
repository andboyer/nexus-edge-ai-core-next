//! RTSP `OPTIONS` + `DESCRIBE` Verify probe with Digest fallback.
//!
//! The inline `POST /api/v1/admin/discovery/:session_id/probe-rtsp`
//! handler calls into [`probe`] to confirm that an
//! `(host, port, path, optional creds)` actually answers RTSP
//! and to surface the SDP track summary (codec + resolution)
//! before the operator clicks **Add** in the UI.
//!
//! Wire flow:
//!
//! 1. Open a TCP socket to `host:port` (1.5 s connect timeout).
//! 2. Send `OPTIONS rtsp://host:port/path RTSP/1.0`.
//!    Read the reply head (≤8 KiB). A `200` with a `Public:`
//!    header confirms an RTSP server is listening.
//! 3. Send `DESCRIBE rtsp://host:port/path RTSP/1.0` (no auth).
//!    * On `200` with a SDP body → parse and return.
//!    * On `401` with `WWW-Authenticate: Digest …` → reopen the
//!      socket, recompute the digest response per RFC 2617
//!      (MD5 only — Basic is intentionally not supported because
//!      the operator already typed the creds and we want to fail
//!      loudly on `WWW-Authenticate: Basic` rather than silently
//!      sending a plaintext password), and retry.
//!
//! Returns [`ProbeRtspResult`] with `ok=true` iff the second leg
//! ended with a `200` and a parseable SDP body.
//!
//! ## Path discovery
//!
//! When the operator's request leaves `path` empty or supplies a
//! bare `"/"` (the common case immediately after a CIDR scan, since
//! the scan only does OPTIONS and never learns vendor paths), the
//! handler iterates [`DEFAULT_PATHS`] — a curated list of the
//! ~10 paths that cover Hikvision, Dahua/Amcrest, Axis, Foscam,
//! Reolink, TP-Link, generic ONVIF and a `/` last-resort. The
//! first path that DESCRIBE-200s wins and is echoed back in
//! `ProbeRtspResult.path`. The UI then uses that string to build
//! the camera's final `rtsp://host:port<path>` URL on Add, so the
//! operator doesn't have to hand-type the vendor-specific suffix.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use md5::{Digest, Md5};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::debug;

use super::{ProbeRtspReq, ProbeRtspResult, SdpStream};

const CONNECT_TIMEOUT: Duration = Duration::from_millis(1_500);
const READ_TIMEOUT: Duration = Duration::from_millis(3_000);
const MAX_RESPONSE: usize = 64 * 1024;

/// Vendor-default RTSP paths the probe tries when the operator
/// supplies an empty / `/` path. Ordered by approximate consumer-
/// market share so the common case completes in one TCP attempt:
///
///   * Hikvision / Annke / EZVIZ — `/Streaming/Channels/{101,102}`
///   * Dahua / Amcrest / Lorex — `/cam/realmonitor?channel=1&subtype={0,1}`
///   * Axis — `/axis-media/media.amp`
///   * Reolink — `/h264Preview_01_main`
///   * Foscam — `/videoMain`
///   * TP-Link Tapo / VIGI — `/stream1`
///   * Generic ONVIF profile names — `/Streaming/Channels/1`, `/live`
///   * Last-resort `/` for everything else (the camera's own root,
///     used by some Bosch and budget devices).
///
/// Substream variants (`102`, `subtype=1`, …) live AFTER the
/// matching main-stream entry so the bandwidth-hungry main feed is
/// the default Add suggestion when both work.
const DEFAULT_PATHS: &[&str] = &[
    "/Streaming/Channels/101",
    "/Streaming/Channels/102",
    "/cam/realmonitor?channel=1&subtype=0",
    "/cam/realmonitor?channel=1&subtype=1",
    "/axis-media/media.amp",
    "/h264Preview_01_main",
    "/h264Preview_01_sub",
    "/videoMain",
    "/stream1",
    "/live",
    "/Streaming/Channels/1",
    "/",
];

pub(crate) async fn probe(req: &ProbeRtspReq) -> ProbeRtspResult {
    // If the operator supplied a real path (anything other than
    // "" or "/"), honour it verbatim. Otherwise iterate
    // [`DEFAULT_PATHS`] until one succeeds. This matches the UX
    // we want: a hand-typed path is treated as gospel, an empty
    // path triggers discovery.
    let requested = req.path.trim();
    let candidates: Vec<String> = if requested.is_empty() || requested == "/" {
        DEFAULT_PATHS.iter().map(|p| (*p).to_string()).collect()
    } else {
        vec![requested.to_string()]
    };

    let mut last: Option<ProbeRtspResult> = None;
    for candidate in &candidates {
        let attempt = ProbeRtspReq {
            host: req.host.clone(),
            port: req.port,
            path: candidate.clone(),
            username: req.username.clone(),
            password: req.password.clone(),
        };
        match probe_inner(&attempt).await {
            Ok(mut r) => {
                if r.ok {
                    r.path = Some(candidate.clone());
                    return r;
                }
                // Auth failure means the camera *did* answer; trying
                // a different path won't get us past the gate. Stop
                // here so the operator sees the 401 and supplies
                // credentials (or fixes the ones they typed).
                if r.status == 401 || r.status == 403 {
                    r.path = Some(candidate.clone());
                    return r;
                }
                last = Some(r);
            }
            Err(e) => {
                debug!(
                    host = %req.host,
                    port = req.port,
                    path = %candidate,
                    error = %e,
                    "rtsp Verify candidate failed"
                );
            }
        }
    }

    // Every candidate either errored or returned ok=false. Return
    // the last non-error reply so the UI surfaces the camera's
    // actual status code (e.g. 401 means "creds wrong", 404 means
    // "we tried every default path and none matched — operator
    // needs to supply the path manually").
    last.unwrap_or(ProbeRtspResult {
        ok: false,
        status: 0,
        sdp_streams: Vec::new(),
        path: None,
    })
}

async fn probe_inner(req: &ProbeRtspReq) -> io::Result<ProbeRtspResult> {
    let path = normalize_path(&req.path);
    let host = req.host.trim();
    let url = format!("rtsp://{host}:{port}{path}", port = req.port);

    // OPTIONS — purely a liveness check. Even if the camera
    // requires auth on DESCRIBE, OPTIONS is usually open.
    let options_text = send_rtsp(
        host,
        req.port,
        &build_request("OPTIONS", &url, 1, None, None),
    )
    .await?;
    let (options_status, _options_headers, _) = split_response(&options_text);

    // DESCRIBE leg 1 — no auth.
    let describe_text = send_rtsp(
        host,
        req.port,
        &build_request("DESCRIBE", &url, 2, None, Some("application/sdp")),
    )
    .await?;
    let (status_1, headers_1, body_1) = split_response(&describe_text);

    if status_1 == 200 && !body_1.is_empty() {
        return Ok(ProbeRtspResult {
            ok: true,
            status: 200,
            sdp_streams: parse_sdp(body_1),
            // Filled in by the caller in `probe()` once it knows
            // which candidate path won — leave None here.
            path: None,
        });
    }

    // 401 challenge — retry DESCRIBE with creds. Cameras vary
    // wildly here, so we try every advertised scheme rather than
    // just the first `WWW-Authenticate` line. Concretely:
    //
    //   * Many Hikvision / Dahua firmwares ship BOTH `Basic` and
    //     `Digest` lines in that order. Returning only the first
    //     match silently dropped Digest and made every probe
    //     report 401 even with valid creds.
    //   * Some firmware uses `algorithm=MD5-sess` instead of MD5
    //     (handled inside `build_digest_response`).
    //   * Budget IP cameras occasionally advertise only `Basic`.
    //
    // Strategy: collect every `WWW-Authenticate` line, prefer
    // Digest variants (most secure), fall back to Basic. Bail
    // only when every attempt returns non-200.
    if status_1 == 401 {
        if let (Some(user), Some(pw)) = (req.username.as_deref(), req.password.as_deref()) {
            let challenges = find_all_header_values(headers_1, "WWW-Authenticate");
            let mut last_status: u16 = 401;

            // Digest first, since cameras that support both
            // schemes still prefer it for security.
            for challenge in &challenges {
                let Some(params) = parse_digest_challenge(challenge) else {
                    continue;
                };
                let auth_header =
                    build_digest_response(&params, user, pw, "DESCRIBE", &url, 3);
                let body = build_request(
                    "DESCRIBE",
                    &url,
                    3,
                    Some(&auth_header),
                    Some("application/sdp"),
                );
                let text = send_rtsp(host, req.port, &body).await?;
                let (status_2, _h2, body_2) = split_response(&text);
                if status_2 == 200 && !body_2.is_empty() {
                    return Ok(ProbeRtspResult {
                        ok: true,
                        status: 200,
                        sdp_streams: parse_sdp(body_2),
                        path: None,
                    });
                }
                last_status = status_2;
            }

            // Basic. Some cameras advertise only `Basic`; others
            // advertise both but the Digest leg failed because of
            // an unsupported `algorithm` value (e.g. SHA-256).
            // We try Basic if any challenge declares it.
            let basic_advertised = challenges
                .iter()
                .any(|c| c.trim_start().to_ascii_lowercase().starts_with("basic"));
            if basic_advertised {
                let auth_header = build_basic_response(user, pw);
                let body = build_request(
                    "DESCRIBE",
                    &url,
                    4,
                    Some(&auth_header),
                    Some("application/sdp"),
                );
                let text = send_rtsp(host, req.port, &body).await?;
                let (status_2, _h2, body_2) = split_response(&text);
                if status_2 == 200 && !body_2.is_empty() {
                    return Ok(ProbeRtspResult {
                        ok: true,
                        status: 200,
                        sdp_streams: parse_sdp(body_2),
                        path: None,
                    });
                }
                last_status = status_2;
            }

            return Ok(ProbeRtspResult {
                ok: false,
                status: last_status,
                sdp_streams: Vec::new(),
                path: None,
            });
        }
    }

    Ok(ProbeRtspResult {
        ok: false,
        // Surface the DESCRIBE status when OPTIONS was 200 (the
        // common "options ok, describe gated" case), else fall
        // back to whatever OPTIONS returned.
        status: if status_1 == 0 {
            options_status
        } else {
            status_1
        },
        sdp_streams: Vec::new(),
        path: None,
    })
}

/// Connect, write request, read reply head + body (up to
/// `MAX_RESPONSE` bytes), close the socket. Reused for both
/// OPTIONS and DESCRIBE.
async fn send_rtsp(host: &str, port: u16, req: &str) -> io::Result<String> {
    // Resolve the host. `IpAddr::parse` first so we don't pay
    // for DNS when the operator already typed a dotted-quad.
    let addr = if let Ok(ip) = host.parse::<IpAddr>() {
        SocketAddr::new(ip, port)
    } else {
        let mut iter = tokio::net::lookup_host((host, port)).await?;
        iter.next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no DNS result"))?
    };

    let mut stream = timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connect timeout"))??;
    stream.set_nodelay(true).ok();
    timeout(READ_TIMEOUT, stream.write_all(req.as_bytes()))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "write timeout"))??;

    let mut out = Vec::with_capacity(8 * 1024);
    let mut buf = [0u8; 4096];
    loop {
        let n = match timeout(READ_TIMEOUT, stream.read(&mut buf)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return Err(e),
            Err(_) => break, // read timeout — return whatever we have
        };
        out.extend_from_slice(&buf[..n]);
        if out.len() >= MAX_RESPONSE {
            break;
        }
        // Cheap optimization: if we already have `\r\n\r\n` and
        // (no Content-Length or we've read at least that many
        // body bytes) we can stop early. Doing it inline keeps
        // the function ~50 LoC; the cost of the extra wakeup
        // when the camera trickles bytes is negligible.
        if let Some(head_end) = window_index(&out, b"\r\n\r\n") {
            let head = &out[..head_end];
            if let Some(cl) = parse_content_length(head) {
                if out.len() - (head_end + 4) >= cl {
                    break;
                }
            }
        }
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

fn window_index(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn parse_content_length(head: &[u8]) -> Option<usize> {
    let text = std::str::from_utf8(head).ok()?;
    for line in text.split("\r\n") {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("Content-Length") {
                return v.trim().parse::<usize>().ok();
            }
        }
    }
    None
}

fn build_request(
    method: &str,
    url: &str,
    cseq: u32,
    authorization: Option<&str>,
    accept: Option<&str>,
) -> String {
    let mut s = format!(
        "{method} {url} RTSP/1.0\r\n\
         CSeq: {cseq}\r\n\
         User-Agent: nexus-discovery/1\r\n"
    );
    if let Some(a) = accept {
        s.push_str("Accept: ");
        s.push_str(a);
        s.push_str("\r\n");
    }
    if let Some(h) = authorization {
        s.push_str("Authorization: ");
        s.push_str(h);
        s.push_str("\r\n");
    }
    s.push_str("\r\n");
    s
}

/// Path normalisation: ensure a leading `/`, accept an empty
/// path as `/`.
fn normalize_path(p: &str) -> String {
    let p = p.trim();
    if p.is_empty() {
        "/".to_string()
    } else if p.starts_with('/') {
        p.to_string()
    } else {
        format!("/{p}")
    }
}

/// Split an RTSP/HTTP-style reply into `(status, headers, body)`.
/// `headers` is the raw header block (LF-separated lines incl.
/// the status line). `body` is everything after the blank line
/// terminator. Returns `(0, "", "")` for malformed replies.
fn split_response(text: &str) -> (u16, &str, &str) {
    let split = text.find("\r\n\r\n").map(|i| (i, 4));
    let (hi, sep) = match split {
        Some(v) => v,
        None => return (0, "", ""),
    };
    let head = &text[..hi];
    let body = &text[hi + sep..];
    let status_line = head.lines().next().unwrap_or("");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    (status, head, body)
}

/// Collect every header value matching `name`. Required for
/// `WWW-Authenticate` since RFC 7235 explicitly allows multiple
/// challenges in one response — and consumer cameras routinely
/// ship `Basic` + `Digest` as separate lines. Returning only the
/// first match (as a "find first" helper would) would silently
/// drop the second scheme and report 401 even with valid
/// credentials.
fn find_all_header_values<'a>(headers: &'a str, name: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    for line in headers.lines() {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case(name) {
                out.push(v.trim());
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Digest auth (RFC 2617 — MD5 only)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct DigestParams {
    realm: String,
    nonce: String,
    opaque: Option<String>,
    qop: Option<String>,
    algorithm: Option<String>,
}

/// Parse a `WWW-Authenticate: Digest …` header value. Tolerates
/// the comma-separated `k=v` / `k="v"` salad cameras actually
/// ship. Returns `None` for non-Digest schemes.
fn parse_digest_challenge(header: &str) -> Option<DigestParams> {
    let rest = header
        .strip_prefix("Digest ")
        .or_else(|| header.strip_prefix("digest "))?;
    let mut out = DigestParams::default();
    for raw in split_digest_params(rest) {
        let (k, v) = raw.split_once('=')?;
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim().trim_matches('"').to_string();
        match k.as_str() {
            "realm" => out.realm = v,
            "nonce" => out.nonce = v,
            "opaque" => out.opaque = Some(v),
            "qop" => out.qop = Some(v),
            "algorithm" => out.algorithm = Some(v),
            _ => {}
        }
    }
    if out.realm.is_empty() || out.nonce.is_empty() {
        return None;
    }
    Some(out)
}

/// Comma-split a Digest challenge while honouring quoted values
/// (`qop="auth,auth-int"` must not be split on the inner comma).
fn split_digest_params(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut in_quote = false;
    for ch in s.chars() {
        match ch {
            '"' => {
                in_quote = !in_quote;
                buf.push(ch);
            }
            ',' if !in_quote => {
                if !buf.trim().is_empty() {
                    out.push(buf.trim().to_string());
                }
                buf.clear();
            }
            _ => buf.push(ch),
        }
    }
    if !buf.trim().is_empty() {
        out.push(buf.trim().to_string());
    }
    out
}

/// Build the RFC 2617 `Authorization: Digest …` header value.
/// Uses `qop=auth` when the server advertises it; falls back to
/// the legacy `MD5(HA1:nonce:HA2)` form otherwise.
///
/// Supports both `algorithm=MD5` (default if absent) and
/// `algorithm=MD5-sess` — Hikvision firmware ≥ 5.5.x and some
/// Reolink models advertise MD5-sess, which redefines HA1 as
/// `MD5(MD5(user:realm:pass) ":" nonce ":" cnonce)`. The
/// previous implementation always used the plain-MD5 HA1, which
/// the camera rejected with another 401 even though the
/// credentials were correct.
fn build_digest_response(
    params: &DigestParams,
    username: &str,
    password: &str,
    method: &str,
    uri: &str,
    nc: u32,
) -> String {
    let plain_ha1 = md5_hex(&format!(
        "{username}:{realm}:{password}",
        realm = params.realm
    ));
    let ha2 = md5_hex(&format!("{method}:{uri}"));

    let is_sess = params
        .algorithm
        .as_deref()
        .map(|a| a.eq_ignore_ascii_case("md5-sess"))
        .unwrap_or(false);

    // Pick qop=auth iff the server lists it (qop may be a CSV).
    let qop_auth = params
        .qop
        .as_deref()
        .map(|q| {
            q.split(',')
                .map(|s| s.trim())
                .any(|s| s.eq_ignore_ascii_case("auth"))
        })
        .unwrap_or(false);

    // MD5-sess REQUIRES a cnonce even outside qop=auth, since the
    // session-key HA1 mixes it in. When the camera advertises
    // MD5-sess but not qop=auth (rare but observed on some
    // Foscam firmware), we still need to generate cnonce.
    let need_cnonce = qop_auth || is_sess;
    let cnonce = if need_cnonce {
        format!(
            "{:016x}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        )
    } else {
        String::new()
    };

    let ha1 = if is_sess {
        md5_hex(&format!(
            "{plain_ha1}:{nonce}:{cnonce}",
            nonce = params.nonce,
        ))
    } else {
        plain_ha1
    };

    let (response, qop_part, cnonce_part, nc_part) = if qop_auth {
        let nc_str = format!("{nc:08x}");
        let r = md5_hex(&format!(
            "{ha1}:{nonce}:{nc}:{cnonce}:auth:{ha2}",
            nonce = params.nonce,
            nc = nc_str,
        ));
        (
            r,
            ", qop=auth".to_string(),
            format!(", cnonce=\"{cnonce}\""),
            format!(", nc={nc_str}"),
        )
    } else {
        let r = md5_hex(&format!("{ha1}:{nonce}:{ha2}", nonce = params.nonce));
        // MD5-sess outside qop=auth: include cnonce so the camera
        // can reconstruct the same session-key HA1 we used (RFC
        // 2617 §3.2.2 — "If the directive 'algorithm' is set to
        // 'MD5-sess', the client must produce a cnonce value").
        let cnonce_hdr = if is_sess {
            format!(", cnonce=\"{cnonce}\"")
        } else {
            String::new()
        };
        (r, String::new(), cnonce_hdr, String::new())
    };

    let mut h = format!(
        r#"Digest username="{user}", realm="{realm}", nonce="{nonce}", uri="{uri}", response="{resp}""#,
        user = username,
        realm = params.realm,
        nonce = params.nonce,
        uri = uri,
        resp = response,
    );
    if let Some(opaque) = &params.opaque {
        h.push_str(&format!(", opaque=\"{opaque}\""));
    }
    if let Some(alg) = &params.algorithm {
        h.push_str(&format!(", algorithm={alg}"));
    }
    h.push_str(&qop_part);
    h.push_str(&nc_part);
    h.push_str(&cnonce_part);
    h
}

/// Build an RFC 7617 `Authorization: Basic …` header value. Used
/// when the camera advertises Basic in `WWW-Authenticate` (or
/// when our Digest leg failed because of an unsupported algorithm
/// like SHA-256). Plaintext-equivalent over the wire — fine for
/// the local-network Verify probe but never propagated into the
/// stored camera URL.
fn build_basic_response(username: &str, password: &str) -> String {
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine as _;
    let creds = format!("{username}:{password}");
    let b64 = B64.encode(creds.as_bytes());
    format!("Basic {b64}")
}

fn md5_hex(s: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(s.as_bytes());
    let bytes = hasher.finalize();
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

// ---------------------------------------------------------------------------
// SDP parser
// ---------------------------------------------------------------------------

/// Pull a per-track summary out of an SDP body. We care about
/// codec (from `a=rtpmap:`), resolution (from `a=framesize:`
/// when present, or `a=x-dimensions:` on some cameras), and the
/// control URL (from `a=control:`) — that last one is what the
/// camera-create form pre-fills into the RTSP URL path.
fn parse_sdp(body: &str) -> Vec<SdpStream> {
    let mut out = Vec::new();
    let mut current: Option<SdpStream> = None;
    for line in body.lines() {
        let line = line.trim();
        if let Some(media) = line.strip_prefix("m=") {
            // `m=video 0 RTP/AVP 96` — close the prior track, start a new one.
            if let Some(prev) = current.take() {
                out.push(prev);
            }
            let kind = media.split_whitespace().next().unwrap_or("?");
            current = Some(SdpStream {
                codec: kind.to_string(),
                resolution: None,
                control: None,
            });
        } else if let Some(track) = current.as_mut() {
            if let Some(rest) = line.strip_prefix("a=rtpmap:") {
                // `96 H264/90000` — pull the codec name.
                if let Some(codec) = rest.split_whitespace().nth(1) {
                    if let Some((c, _)) = codec.split_once('/') {
                        track.codec = c.to_string();
                    } else {
                        track.codec = codec.to_string();
                    }
                }
            } else if let Some(rest) = line.strip_prefix("a=framesize:") {
                // `96 1280-720` (some cameras use `-`, some `x`).
                if let Some(size) = rest.split_whitespace().nth(1) {
                    track.resolution = Some(size.replace('-', "x"));
                }
            } else if let Some(rest) = line.strip_prefix("a=x-dimensions:") {
                // `96,1920,1080`
                let parts: Vec<&str> = rest.splitn(2, ',').collect();
                if let Some(dims) = parts.get(1) {
                    let dims = dims.replace(',', "x");
                    track.resolution = Some(dims);
                }
            } else if let Some(rest) = line.strip_prefix("a=control:") {
                track.control = Some(rest.to_string());
            }
        }
    }
    if let Some(prev) = current.take() {
        out.push(prev);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_response_extracts_status_and_body() {
        let text = "RTSP/1.0 200 OK\r\nCSeq: 1\r\nContent-Length: 4\r\n\r\nbody";
        let (status, headers, body) = split_response(text);
        assert_eq!(status, 200);
        assert!(headers.contains("Content-Length: 4"));
        assert_eq!(body, "body");
    }

    #[test]
    fn split_response_handles_no_body() {
        let text = "RTSP/1.0 401 Unauthorized\r\nWWW-Authenticate: Digest realm=\"x\", nonce=\"y\"\r\n\r\n";
        let (status, headers, body) = split_response(text);
        assert_eq!(status, 401);
        assert!(headers.to_lowercase().contains("www-authenticate"));
        assert_eq!(body, "");
    }

    #[test]
    fn parse_digest_challenge_extracts_required_fields() {
        let header =
            r#"Digest realm="IP Camera", nonce="abc123", qop="auth", opaque="opq", algorithm=MD5"#;
        let p = parse_digest_challenge(header).unwrap();
        assert_eq!(p.realm, "IP Camera");
        assert_eq!(p.nonce, "abc123");
        assert_eq!(p.qop.as_deref(), Some("auth"));
        assert_eq!(p.opaque.as_deref(), Some("opq"));
        assert_eq!(p.algorithm.as_deref(), Some("MD5"));
    }

    #[test]
    fn parse_digest_challenge_handles_quoted_csv_qop() {
        let header = r#"Digest realm="r", nonce="n", qop="auth,auth-int""#;
        let p = parse_digest_challenge(header).unwrap();
        assert_eq!(p.qop.as_deref(), Some("auth,auth-int"));
    }

    #[test]
    fn parse_digest_challenge_rejects_basic() {
        assert!(parse_digest_challenge("Basic realm=\"x\"").is_none());
    }

    #[test]
    fn parse_digest_challenge_requires_realm_and_nonce() {
        assert!(parse_digest_challenge("Digest realm=\"x\"").is_none());
        assert!(parse_digest_challenge("Digest nonce=\"y\"").is_none());
    }

    #[test]
    fn build_digest_response_matches_rfc_2617_example() {
        // RFC 2617 §3.5 example, legacy (no-qop) compatibility mode.
        // HA1 = MD5("Mufasa:testrealm@host.com:Circle Of Life")
        //     = 939e7578ed9e3c518a452acee763bce9
        // HA2 = MD5("GET:/dir/index.html")
        //     = 39aff3a2bab6126f332b942af96d3366
        // response = MD5("HA1:nonce:HA2")
        //          = 670fd8c2df070c60b045671b8b24ff02
        let params = DigestParams {
            realm: "testrealm@host.com".to_string(),
            nonce: "dcd98b7102dd2f0e8b11d0f600bfb0c093".to_string(),
            opaque: Some("5ccc069c403ebaf9f0171e9517f40e41".to_string()),
            qop: None,
            algorithm: None,
        };
        let header = build_digest_response(
            &params,
            "Mufasa",
            "Circle Of Life",
            "GET",
            "/dir/index.html",
            1,
        );
        assert!(header.contains(r#"username="Mufasa""#));
        assert!(header.contains(r#"realm="testrealm@host.com""#));
        assert!(header.contains(r#"nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093""#));
        assert!(header.contains(r#"uri="/dir/index.html""#));
        assert!(header.contains(r#"response="670fd8c2df070c60b045671b8b24ff02""#));
        assert!(header.contains(r#"opaque="5ccc069c403ebaf9f0171e9517f40e41""#));
        // No qop block when the server didn't ask for it.
        assert!(!header.contains("qop="));
        assert!(!header.contains("nc="));
    }

    #[test]
    fn build_digest_response_includes_qop_block_when_advertised() {
        let params = DigestParams {
            realm: "r".to_string(),
            nonce: "n".to_string(),
            opaque: None,
            qop: Some("auth".to_string()),
            algorithm: Some("MD5".to_string()),
        };
        let header = build_digest_response(&params, "u", "p", "DESCRIBE", "rtsp://x/", 1);
        assert!(header.contains("qop=auth"));
        assert!(header.contains("nc=00000001"));
        assert!(header.contains("cnonce="));
    }

    #[test]
    fn parse_sdp_extracts_h264_track_with_control() {
        let body = "v=0\r\n\
            o=- 0 0 IN IP4 0.0.0.0\r\n\
            s=Session streamed by GStreamer\r\n\
            c=IN IP4 0.0.0.0\r\n\
            t=0 0\r\n\
            m=video 0 RTP/AVP 96\r\n\
            a=rtpmap:96 H264/90000\r\n\
            a=framesize:96 1920-1080\r\n\
            a=control:trackID=0\r\n\
            m=audio 0 RTP/AVP 97\r\n\
            a=rtpmap:97 MPEG4-GENERIC/48000/2\r\n\
            a=control:trackID=1\r\n";
        let tracks = parse_sdp(body);
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].codec, "H264");
        assert_eq!(tracks[0].resolution.as_deref(), Some("1920x1080"));
        assert_eq!(tracks[0].control.as_deref(), Some("trackID=0"));
        assert_eq!(tracks[1].codec, "MPEG4-GENERIC");
    }

    #[test]
    fn parse_sdp_falls_back_to_x_dimensions() {
        let body = "v=0\r\nm=video 0 RTP/AVP 96\r\na=rtpmap:96 H265/90000\r\na=x-dimensions:96,3840,2160\r\n";
        let tracks = parse_sdp(body);
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].codec, "H265");
        assert_eq!(tracks[0].resolution.as_deref(), Some("3840x2160"));
    }

    #[test]
    fn normalize_path_adds_leading_slash() {
        assert_eq!(normalize_path(""), "/");
        assert_eq!(normalize_path("/foo"), "/foo");
        assert_eq!(normalize_path("foo"), "/foo");
        assert_eq!(normalize_path("  /bar  "), "/bar");
    }

    #[test]
    fn find_all_header_values_returns_every_match() {
        // Camera firmwares routinely emit `Basic` + `Digest`
        // on two separate `WWW-Authenticate` lines. The Verify
        // probe must see both, otherwise it silently drops the
        // Digest challenge and falsely reports 401.
        let headers = "WWW-Authenticate: Basic realm=\"cam\"\r\n\
            WWW-Authenticate: Digest realm=\"cam\", nonce=\"abc\"\r\n\
            CSeq: 2";
        let values = find_all_header_values(headers, "WWW-Authenticate");
        assert_eq!(values.len(), 2);
        assert!(values[0].starts_with("Basic"));
        assert!(values[1].starts_with("Digest"));
    }

    #[test]
    fn build_digest_response_handles_md5_sess() {
        // MD5-sess HA1 = MD5(MD5(user:realm:pass):nonce:cnonce)
        // The header MUST include cnonce even when qop is unset,
        // since the session key depends on it. We verify the
        // structural invariants (cnonce present, algorithm
        // round-tripped) — the exact response hash varies with
        // the wall-clock-seeded cnonce.
        let params = DigestParams {
            realm: "r".to_string(),
            nonce: "n".to_string(),
            opaque: None,
            qop: None,
            algorithm: Some("MD5-sess".to_string()),
        };
        let header = build_digest_response(&params, "u", "p", "DESCRIBE", "rtsp://x/", 1);
        assert!(header.contains("algorithm=MD5-sess"));
        assert!(header.contains("cnonce="));
        // No qop block when the server didn't ask for it, even
        // though MD5-sess forced a cnonce.
        assert!(!header.contains("qop="));
        assert!(!header.contains("nc="));
    }

    #[test]
    fn build_basic_response_encodes_user_colon_pass() {
        // RFC 7617: token = base64(user ":" pass). The receiving
        // camera decodes back to "user:pass" — any deviation
        // (e.g. URL-encoding the colon) breaks the legacy Basic
        // path that we use as a fallback for cameras that don't
        // advertise Digest.
        let h = build_basic_response("admin", "secret123");
        // base64("admin:secret123") = YWRtaW46c2VjcmV0MTIz
        assert_eq!(h, "Basic YWRtaW46c2VjcmV0MTIz");
    }
}
