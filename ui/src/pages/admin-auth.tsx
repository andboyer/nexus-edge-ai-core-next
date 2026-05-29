// Admin Auth Configuration page.
//
// Surfaces:
//   * the currently-active auth mode + capability flags (read-only),
//   * a pending-restart editor for `auth.mode` + the OIDC issuer/audience
//     (restart-based — engine snapshots at boot, see admin_runtime).
//   * a discovery dry-run probe so operators can validate an OIDC issuer
//     URL *before* persisting it.
//
// What we deliberately do NOT expose in the UI:
//   * `client_secret` / `client_secret_file` / `client_secret_env`. Those
//     stay in the config file (or the env). Echoing them back to the
//     browser would be a regression even though only admins reach this
//     page — defense in depth.
//   * The full `role_map`. Phase 6 keeps role mapping config-driven; the
//     UI just shows the current claim list so operators can sanity-check
//     it. A dedicated role-mapper editor is out of scope here.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  CheckCircle2,
  KeyRound,
  Save,
  Search,
  ShieldCheck,
  XCircle,
} from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { toast } from "sonner";

import type { AuthMode, TestDiscoveryOut } from "@/api/admin";
import {
  getAuthConfig,
  putAuthConfig,
  testOidcDiscovery,
} from "@/api/admin";
import { authApi } from "@/api/auth";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";

const MODE_OPTIONS: { value: AuthMode; label: string; help: string }[] = [
  { value: "local", label: "Local password", help: "Admin-issued credentials stored in the engine SQLite DB." },
  { value: "oidc", label: "OIDC SSO", help: "All users authenticate against an external identity provider." },
  { value: "hybrid", label: "Hybrid (OIDC + local)", help: "OIDC by default, local password as a fallback for break-glass admins." },
];

export function AdminAuthPage() {
  const qc = useQueryClient();

  const infoQuery = useQuery({
    queryKey: ["auth", "info"],
    queryFn: () => authApi.info(),
  });

  const configQuery = useQuery({
    queryKey: ["admin", "auth", "config"],
    queryFn: () => getAuthConfig(),
  });

  // Editor state. We seed once from the persisted-pending value if the
  // operator has already queued a change, otherwise from the active
  // config. Subsequent re-renders of the query data don't clobber the
  // operator's in-progress edit.
  const [mode, setMode] = useState<AuthMode>("local");
  const [issuer, setIssuer] = useState("");
  const [audience, setAudience] = useState("");
  const [clientId, setClientId] = useState("");
  const [seeded, setSeeded] = useState(false);

  useEffect(() => {
    if (!configQuery.data || seeded) return;
    const source = configQuery.data.pending ?? configQuery.data.current;
    setMode(source.mode);
    setIssuer(source.oidc?.issuer ?? "");
    setAudience(source.oidc?.audience ?? "");
    setClientId(source.oidc?.client_id ?? "");
    setSeeded(true);
  }, [configQuery.data, seeded]);

  const needsOidc = mode === "oidc" || mode === "hybrid";

  // Discovery dry-run state. Independent of the Save mutation — operators
  // can run the probe multiple times before committing.
  const [discovery, setDiscovery] = useState<TestDiscoveryOut | null>(null);
  const discoveryMutation = useMutation({
    mutationFn: () =>
      testOidcDiscovery({
        issuer: issuer.trim(),
        audience: audience.trim() || undefined,
      }),
    onSuccess: (res) => {
      setDiscovery(res);
      toast.success(`Discovery OK: ${res.issuer}`);
    },
    onError: (e: unknown) => {
      setDiscovery(null);
      const msg = e instanceof Error ? e.message : String(e);
      toast.error(`Discovery failed: ${msg}`);
    },
  });

  const saveMutation = useMutation({
    mutationFn: () => {
      // Build the wire `AuthConfig` shape. Secrets are intentionally
      // NOT round-tripped from the form — the operator keeps those in
      // the config file or env, the engine reads them at boot.
      const body: Record<string, unknown> = { mode };
      if (needsOidc) {
        body.oidc = {
          issuer: issuer.trim(),
          audience: audience.trim() || issuer.trim(),
          client_id: clientId.trim() || null,
          // Scopes/role_claims/role_map use server-side defaults. A
          // future iteration can surface them as advanced fields.
        };
      }
      return putAuthConfig(body);
    },
    onSuccess: (res) => {
      toast.success(
        `Auth config saved (mode=${res.mode}). Restart engine to apply.`,
      );
      qc.invalidateQueries({ queryKey: ["admin", "auth", "config"] });
    },
    onError: (e: unknown) => {
      const msg = e instanceof Error ? e.message : String(e);
      toast.error(`Failed to save auth config: ${msg}`);
    },
  });

  const pendingDiffers = useMemo(() => {
    const pending = configQuery.data?.pending;
    const current = configQuery.data?.current;
    if (!pending || !current) return false;
    if (pending.mode !== current.mode) return true;
    const po = pending.oidc;
    const co = current.oidc;
    if ((po && !co) || (!po && co)) return true;
    if (po && co) {
      if (po.issuer !== co.issuer) return true;
      if (po.audience !== co.audience) return true;
      if (po.client_id !== co.client_id) return true;
    }
    return false;
  }, [configQuery.data]);

  return (
    <div className="space-y-6">
      <header>
        <h1 className="text-2xl font-semibold">Auth configuration</h1>
        <p className="text-sm text-muted-foreground">
          Active mode applies until the next engine restart. Edits below
          are validated server-side (OIDC discovery is dry-run before
          persist) and take effect on next boot.
        </p>
      </header>

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2 text-base">
            <ShieldCheck className="h-4 w-4 text-muted-foreground" />
            Active mode
          </CardTitle>
        </CardHeader>
        <CardContent>
          {infoQuery.isLoading ? (
            <Skeleton className="h-16 w-full" />
          ) : infoQuery.data ? (
            <div className="space-y-3 text-sm">
              <Row
                k="Mode"
                v={
                  <Badge variant="outline" className="uppercase">
                    {infoQuery.data.mode}
                  </Badge>
                }
              />
              <Row
                k="Local password login"
                v={<BoolBadge on={infoQuery.data.allows_local} />}
              />
              <Row
                k="OIDC SSO"
                v={<BoolBadge on={infoQuery.data.allows_oidc} />}
              />
              {infoQuery.data.oidc_display_name ? (
                <Row
                  k="OIDC display name"
                  v={
                    <code className="font-mono">
                      {infoQuery.data.oidc_display_name}
                    </code>
                  }
                />
              ) : null}
            </div>
          ) : null}
        </CardContent>
      </Card>

      {pendingDiffers ? (
        <div
          className="rounded-md border border-warning/40 bg-warning/10 p-3 text-xs"
          data-testid="auth-pending"
        >
          <p className="font-medium text-warning">Pending restart</p>
          <p className="mt-0.5 text-muted-foreground">
            A new auth config is persisted but won't apply until the engine
            restarts. Pending mode:{" "}
            <code className="font-mono">
              {configQuery.data?.pending?.mode}
            </code>
            {configQuery.data?.pending?.oidc?.issuer ? (
              <>
                , issuer{" "}
                <code className="font-mono">
                  {configQuery.data.pending.oidc.issuer}
                </code>
              </>
            ) : null}
            .
          </p>
        </div>
      ) : null}

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2 text-base">
            <KeyRound className="h-4 w-4 text-muted-foreground" />
            Change auth mode
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-4 text-sm">
          {configQuery.isLoading ? (
            <Skeleton className="h-32 w-full" />
          ) : (
            <form
              className="space-y-4"
              onSubmit={(e) => {
                e.preventDefault();
                if (needsOidc && !issuer.trim()) {
                  toast.error("OIDC mode requires an issuer URL");
                  return;
                }
                saveMutation.mutate();
              }}
            >
              <div className="space-y-1">
                <Label htmlFor="auth-mode">Mode</Label>
                <select
                  id="auth-mode"
                  className="h-9 w-full rounded-md border border-input bg-background px-3 py-1 text-sm shadow-sm"
                  value={mode}
                  onChange={(e) => setMode(e.target.value as AuthMode)}
                  data-testid="auth-mode-select"
                >
                  {MODE_OPTIONS.map((opt) => (
                    <option key={opt.value} value={opt.value}>
                      {opt.label}
                    </option>
                  ))}
                </select>
                <p className="text-xs text-muted-foreground">
                  {MODE_OPTIONS.find((o) => o.value === mode)?.help}
                </p>
              </div>

              {needsOidc ? (
                <div className="space-y-3 rounded-md border border-border/60 bg-muted/20 p-3">
                  <div className="space-y-1">
                    <Label htmlFor="oidc-issuer">Issuer URL</Label>
                    <Input
                      id="oidc-issuer"
                      placeholder="https://login.example.com/realms/nexus"
                      value={issuer}
                      onChange={(e) => setIssuer(e.target.value)}
                      data-testid="oidc-issuer-input"
                    />
                  </div>
                  <div className="space-y-1">
                    <Label htmlFor="oidc-audience">Audience</Label>
                    <Input
                      id="oidc-audience"
                      placeholder="nexus-console"
                      value={audience}
                      onChange={(e) => setAudience(e.target.value)}
                      data-testid="oidc-audience-input"
                    />
                    <p className="text-xs text-muted-foreground">
                      Optional. Defaults to the issuer URL.
                    </p>
                  </div>
                  <div className="space-y-1">
                    <Label htmlFor="oidc-client-id">Client ID</Label>
                    <Input
                      id="oidc-client-id"
                      placeholder="nexus-console"
                      value={clientId}
                      onChange={(e) => setClientId(e.target.value)}
                      data-testid="oidc-client-id-input"
                    />
                    <p className="text-xs text-muted-foreground">
                      Optional. Required only if the engine initiates the
                      auth-code flow on behalf of the SPA.
                    </p>
                  </div>
                  <div className="flex flex-col gap-2 sm:flex-row sm:items-end">
                    <Button
                      type="button"
                      variant="outline"
                      onClick={() => discoveryMutation.mutate()}
                      disabled={
                        !issuer.trim() || discoveryMutation.isPending
                      }
                      data-testid="oidc-test-discovery"
                    >
                      <Search className="mr-2 h-4 w-4" />
                      {discoveryMutation.isPending
                        ? "Probing…"
                        : "Test discovery"}
                    </Button>
                    {discovery ? (
                      <p
                        className="text-xs text-muted-foreground"
                        data-testid="oidc-discovery-ok"
                      >
                        Resolved · auth endpoint:{" "}
                        <code className="font-mono">
                          {discovery.authorization_endpoint}
                        </code>
                        {discovery.supports_pkce_s256
                          ? " · PKCE S256 supported"
                          : ""}
                      </p>
                    ) : null}
                  </div>
                </div>
              ) : null}

              <div className="flex justify-end">
                <Button
                  type="submit"
                  disabled={saveMutation.isPending}
                  data-testid="auth-save"
                >
                  <Save className="mr-2 h-4 w-4" />
                  {saveMutation.isPending ? "Saving…" : "Save"}
                </Button>
              </div>
            </form>
          )}

          <div className="rounded-md border border-border/40 bg-muted/20 p-3 text-xs text-muted-foreground">
            <p>
              Secrets (OIDC client_secret, admin_secret_path) are managed
              in <code className="font-mono">nexus.toml</code> or the
              environment, never via the UI. After saving here, restart
              the engine to pick up the new config.
            </p>
          </div>
        </CardContent>
      </Card>
    </div>
  );
}

function Row({ k, v }: { k: string; v: React.ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-3 border-b border-border/30 py-1.5 last:border-b-0">
      <span className="text-muted-foreground">{k}</span>
      <div className="text-right">{v}</div>
    </div>
  );
}

function BoolBadge({ on }: { on: boolean }) {
  return on ? (
    <Badge variant="success">
      <CheckCircle2 className="mr-1 h-3 w-3" />
      enabled
    </Badge>
  ) : (
    <Badge variant="secondary">
      <XCircle className="mr-1 h-3 w-3" />
      disabled
    </Badge>
  );
}
