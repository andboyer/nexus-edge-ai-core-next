// M7 Step 6F2 — mock webhook server for the Playwright e2e suite.
//
// Spawned by globalSetup as a separate Node process; killed by
// globalTeardown. The dispatcher POSTs alert payloads here, and the
// e2e specs poll the request counter via a side-channel.
//
// Routes:
//   POST /webhook   → counter++, return 200 OK (idempotent body)
//   GET  /_count    → JSON `{ count: N, last_event_id: "..." | null }`
//   POST /_reset    → counter = 0, return 200
//
// Why a separate process (not in-test): the engine has a single
// configured webhook URL pinned at boot. If the URL changed
// between specs each spec would need its own engine reboot. A
// stable mock URL + a reset endpoint sidesteps that and lets all
// three specs share one engine + one mock instance.
//
// Standalone runnable: `node mock-webhook-server.mjs <port>`. The
// port is taken from argv so globalSetup can pre-pick a free port
// the same way it does for the engine. Listens only on 127.0.0.1.

import { createServer } from "node:http";

const port = Number.parseInt(process.argv[2] ?? "0", 10);
if (!Number.isFinite(port) || port <= 0) {
  console.error("[mock-webhook] usage: node mock-webhook-server.mjs <port>");
  process.exit(2);
}

let count = 0;
let lastEventId = null;
const bodies = []; // for debugging only; cap below

const server = createServer((req, res) => {
  // POST /webhook — the engine dispatcher hits this with the
  // serialised AlertEvent JSON in the body. We don't care about
  // the contents for assertion purposes; just count and 200.
  if (req.method === "POST" && (req.url === "/webhook" || req.url?.startsWith("/webhook?"))) {
    let raw = "";
    req.on("data", (chunk) => {
      raw += chunk;
      if (raw.length > 1_000_000) {
        // Truncate runaway payloads so a buggy test can't blow heap.
        raw = raw.slice(0, 1_000_000);
      }
    });
    req.on("end", () => {
      count += 1;
      try {
        const parsed = JSON.parse(raw);
        if (parsed && typeof parsed.event_id === "string") {
          lastEventId = parsed.event_id;
        }
      } catch {
        // Not JSON — fine, dispatcher could have wrapped it.
      }
      if (bodies.length < 32) bodies.push(raw);
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ ok: true }));
    });
    return;
  }

  // GET /_count — side-channel readback for specs.
  if (req.method === "GET" && req.url === "/_count") {
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ count, last_event_id: lastEventId }));
    return;
  }

  // POST /_reset — clear state between specs.
  if (req.method === "POST" && req.url === "/_reset") {
    count = 0;
    lastEventId = null;
    bodies.length = 0;
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ ok: true }));
    return;
  }

  // POST /_dump — debug aid; returns the most recent bodies.
  if (req.method === "GET" && req.url === "/_dump") {
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ count, bodies }));
    return;
  }

  res.writeHead(404, { "content-type": "application/json" });
  res.end(JSON.stringify({ error: "not found", method: req.method, url: req.url }));
});

server.listen(port, "127.0.0.1", () => {
  // Mark readiness on stdout — globalSetup tails this until it
  // sees "ready" before treating the mock server as up.
  console.log(`[mock-webhook] ready on 127.0.0.1:${port}`);
});

// Clean exit on SIGTERM (globalTeardown does this).
for (const sig of ["SIGTERM", "SIGINT"]) {
  process.on(sig, () => {
    server.close(() => process.exit(0));
    // Force exit after 2s in case a request is hung.
    setTimeout(() => process.exit(0), 2000).unref();
  });
}
