#!/usr/bin/env bash
# yt-rtsp-bridge — pull a YouTube live stream into a local RTSP feed.
#
# Pipeline: streamlink (preferred) | yt-dlp (fallback) → ffmpeg → mediamtx.
#
# streamlink owns the HLS playlist-refresh loop and per-segment URL signing,
# so ffmpeg never sees a stale googlevideo.com URL go 403. yt-dlp's `-g`
# path hands ffmpeg one signed manifest URL and lets ffmpeg fetch segments
# directly — that breaks within minutes on long-running live streams.
#
# Requirements:
#   brew install streamlink ffmpeg mediamtx     # preferred
#   brew install yt-dlp ffmpeg mediamtx         # fallback
#
# Usage:
#   ./yt-rtsp-bridge.sh <youtube-url> [stream-name]
#
# Then point the engine at:
#   rtsp://127.0.0.1:8554/<stream-name>

set -euo pipefail

URL="${1:?usage: yt-rtsp-bridge.sh <youtube-url> [stream-name]}"
NAME="${2:-cam1}"

PORT="${MEDIAMTX_PORT:-8554}"
RTSP_OUT="rtsp://127.0.0.1:${PORT}/${NAME}"

# Audio is dropped (-an): RTSP rejects AAC without global headers, and the
# engine pipeline only consumes the video track anyway.
if command -v streamlink >/dev/null 2>&1; then
    # streamlink → stdout MPEG-TS → ffmpeg → mediamtx, video re-mux only.
    # -f mpegts + larger probe window so ffmpeg locks onto the live stream
    # before its default 5MB / 5s budget runs out on slow first segments.
    # --ipv4 pins the resolver to A records; YouTube embeds the requester's
    # IP into segment-URL signatures and 403s if the connection path
    # switches between v4/v6 between fetches (common on dual-stack home nets).
    exec streamlink --ipv4 --stdout --default-stream best "$URL" \
        | ffmpeg \
            -hide_banner -loglevel warning \
            -f mpegts -probesize 10M -analyzeduration 10M \
            -re -i pipe:0 \
            -map 0:v:0 -an \
            -c:v copy -f rtsp -rtsp_transport tcp \
            "$RTSP_OUT"
fi

# Fallback: yt-dlp -g + ffmpeg. Works for short tests; can 403 on long lives.
DIRECT_URL="$(yt-dlp -g -f 'best[ext=mp4]/best' "$URL" | head -n1)"
exec ffmpeg \
    -hide_banner -loglevel warning \
    -re -i "$DIRECT_URL" \
    -map 0:v:0 -an \
    -c:v copy -f rtsp -rtsp_transport tcp \
    "$RTSP_OUT"
