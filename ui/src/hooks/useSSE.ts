// EventSource wrapper hook with auto-reconnect.
//
// Why a custom hook (vs a lib): native EventSource already does
// auto-reconnect on transport errors, but it doesn't give us:
//   * an idle/closed/connected status we can render
//   * graceful unmount cleanup that doesn't race the reconnect timer
//   * typed parsed events
//
// The hook reconnects with exponential backoff capped at 30s and
// resets the backoff on a successful `onopen`. Callers pass a
// `parse` function that throws on bad payloads — we swallow + log.

import { useEffect, useRef, useState } from "react";

export type SseStatus = "connecting" | "open" | "closed";

export interface UseSseOptions<T> {
  /** Endpoint URL (relative or absolute). */
  url: string;
  /** Optional parser. Defaults to `JSON.parse`. */
  parse?: (raw: string) => T;
  /** Max number of events to keep in the buffer. Defaults to 50. */
  maxBuffer?: number;
  /** Enable/disable the connection (e.g. `false` when offline). */
  enabled?: boolean;
}

export interface UseSseResult<T> {
  events: T[];
  status: SseStatus;
  lastError: string | null;
  clear: () => void;
}

const INITIAL_BACKOFF_MS = 500;
const MAX_BACKOFF_MS = 30_000;

export function useSSE<T = unknown>(opts: UseSseOptions<T>): UseSseResult<T> {
  const { url, parse, maxBuffer = 50, enabled = true } = opts;
  const [events, setEvents] = useState<T[]>([]);
  const [status, setStatus] = useState<SseStatus>("connecting");
  const [lastError, setLastError] = useState<string | null>(null);
  const sourceRef = useRef<EventSource | null>(null);
  const reconnectTimerRef = useRef<number | null>(null);
  const backoffRef = useRef<number>(INITIAL_BACKOFF_MS);
  const parseRef = useRef(parse);
  parseRef.current = parse;

  useEffect(() => {
    if (!enabled) {
      setStatus("closed");
      return;
    }

    let disposed = false;

    const connect = () => {
      if (disposed) return;
      setStatus("connecting");

      const es = new EventSource(url, { withCredentials: false });
      sourceRef.current = es;

      es.onopen = () => {
        if (disposed) return;
        setStatus("open");
        setLastError(null);
        backoffRef.current = INITIAL_BACKOFF_MS;
      };

      es.onmessage = (evt) => {
        if (disposed) return;
        try {
          const value = (parseRef.current
            ? parseRef.current(evt.data)
            : (JSON.parse(evt.data) as T)) as T;
          setEvents((prev) => {
            const next = [value, ...prev];
            return next.length > maxBuffer ? next.slice(0, maxBuffer) : next;
          });
        } catch (err) {
          // Single bad payload shouldn't tear down the stream.
          // eslint-disable-next-line no-console
          console.warn("[sse] parse error", err);
        }
      };

      es.onerror = () => {
        if (disposed) return;
        setStatus("closed");
        setLastError("connection lost");
        es.close();
        // Schedule a reconnect with exponential backoff.
        const wait = backoffRef.current;
        backoffRef.current = Math.min(wait * 2, MAX_BACKOFF_MS);
        reconnectTimerRef.current = window.setTimeout(connect, wait);
      };
    };

    connect();

    return () => {
      disposed = true;
      if (reconnectTimerRef.current !== null) {
        window.clearTimeout(reconnectTimerRef.current);
        reconnectTimerRef.current = null;
      }
      sourceRef.current?.close();
      sourceRef.current = null;
    };
  }, [url, enabled, maxBuffer]);

  return {
    events,
    status,
    lastError,
    clear: () => setEvents([]),
  };
}
