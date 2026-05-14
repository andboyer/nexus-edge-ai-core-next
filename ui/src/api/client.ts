// Typed fetch wrapper. Every method takes/returns a typed payload from
// `./types.ts`, so callers can never confuse the API shape with the UI's
// own state.

import type {
  AlertEvent,
  BackendsResponse,
  CameraConfig,
  CameraId,
  ClipId,
  MotionEventRow,
  RuleConfig,
  RuleId,
  StorageLocalResponse,
  FrameMetadata,
} from "./types.js";

const BASE = "/api";

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(BASE + path, {
    ...init,
    headers: {
      "Content-Type": "application/json",
      ...(init?.headers ?? {}),
    },
  });
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new Error(`${res.status} ${res.statusText}: ${text}`);
  }
  if (res.status === 204) {
    return undefined as T;
  }
  return (await res.json()) as T;
}

export const api = {
  health: () => request<{ status: string; version: string }>("/health"),

  cameras: {
    list: () => request<CameraConfig[]>("/cameras"),
    upsert: (cam: CameraConfig) =>
      request<CameraConfig>(`/cameras/${cam.id}`, {
        method: "PUT",
        body: JSON.stringify(cam),
      }),
    remove: (id: CameraId) =>
      request<void>(`/cameras/${id}`, { method: "DELETE" }),
    latestSnapshotUrl: (id: CameraId, ts = Date.now()) =>
      `${BASE}/cameras/${id}/frames/latest?t=${ts}`,
    latestMetadata: (id: CameraId) =>
      request<FrameMetadata>(`/cameras/${id}/frames/latest.json`),
  },

  rules: {
    list: () => request<RuleConfig[]>("/rules"),
    upsert: (rule: RuleConfig) =>
      request<RuleConfig>(`/rules/${rule.id}`, {
        method: "PUT",
        body: JSON.stringify(rule),
      }),
    remove: (id: RuleId) =>
      request<void>(`/rules/${id}`, { method: "DELETE" }),
  },

  events: {
    recent: (limit = 100) =>
      request<AlertEvent[]>(`/events?limit=${limit}`),
  },

  backends: () => request<BackendsResponse>("/backends"),

  // M2.1 Stage B (B5) — motion timeline + on-disk clip storage.
  // Binary endpoints return URLs the caller embeds in <video>/<img>;
  // the engine streams them with HTTP Range support so seeking works.
  storage: {
    local: () => request<StorageLocalResponse>("/v1/storage/local"),
  },

  motion: {
    /// Camera-scoped motion event window. `from` / `to` accept
    /// RFC3339 timestamps; the engine clamps `limit` to [1, 5000]
    /// and defaults the window to the last hour.
    listForCamera: (
      cameraId: CameraId,
      opts: { from?: string; to?: string; limit?: number } = {},
    ) => {
      const q = new URLSearchParams();
      if (opts.from) q.set("from", opts.from);
      if (opts.to) q.set("to", opts.to);
      if (opts.limit != null) q.set("limit", String(opts.limit));
      const qs = q.toString();
      const suffix = qs ? `?${qs}` : "";
      return request<MotionEventRow[]>(
        `/v1/cameras/${cameraId}/motion${suffix}`,
      );
    },
  },

  clips: {
    streamUrl: (clipId: ClipId) => `${BASE}/v1/clips/${clipId}`,
    thumbnailUrl: (clipId: ClipId) =>
      `${BASE}/v1/clips/${clipId}/thumbnail`,
  },
};
