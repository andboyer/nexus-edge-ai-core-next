// Admin Network page — OS-level NIC manager.
//
// Three concerns layered top → bottom:
//   1. Live interfaces (read-only snapshot from the OS).
//   2. Ethernets — per-physical-NIC plan editor (dhcp4 / static
//      addrs / gateway / DNS / MTU / MAC override).
//   3. VLANs — add/edit/remove 802.1Q sub-interfaces. The
//      operator needs this to bind the engine on a "secure"
//      VLAN and the admin UI alias on a separate "open" VLAN.
//
// Plan edits persist to `engine_runtime_settings.network_plan_json`
// on every save. They do NOT touch /etc/netplan/* until the
// operator clicks Apply, which mints a 120s rollback timer; the
// operator must re-handshake and click Confirm before the
// deadline or the engine auto-reverts to the prior config.
//
// On non-Linux (macOS dev), Apply returns 501 and the button
// renders disabled with a tooltip.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  AlertTriangle,
  CheckCircle2,
  Network as NetworkIcon,
  Plus,
  RotateCcw,
  Save,
  Trash2,
  Undo2,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";

import type {
  EthernetConfigWire,
  InterfaceKind,
  NameserversWire,
  NetplanPlan,
  NetworkInterface,
  VlanConfigWire,
} from "@/api/admin";
import {
  applyNetworkPlan,
  confirmNetworkApply,
  getNetworkApplyStatus,
  getNetworkPlan,
  listNetworkInterfaces,
  putNetworkPlan,
  rollbackNetworkApply,
} from "@/api/admin";
import { ApiError } from "@/api/client";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function kindBadgeVariant(
  kind: InterfaceKind,
): "default" | "secondary" | "outline" | "destructive" {
  switch (kind) {
    case "physical":
      return "default";
    case "vlan":
      return "secondary";
    case "bridge":
    case "bond":
      return "outline";
    case "loopback":
      return "outline";
    case "wireless":
      return "secondary";
    default:
      return "outline";
  }
}

function fmtAddrs(nic: NetworkInterface): string {
  if (nic.addrs.length === 0) return "—";
  return nic.addrs.map((a) => `${a.addr}/${a.prefix_len}`).join(", ");
}

function operstateBadge(nic: NetworkInterface) {
  const state = nic.operstate ?? "—";
  const carrier = nic.carrier;
  let variant: "default" | "secondary" | "outline" | "destructive" = "outline";
  if (state === "up" && carrier !== false) variant = "default";
  else if (state === "down") variant = "destructive";
  return (
    <Badge variant={variant} className="font-mono text-[10px]">
      {state}
      {carrier === false ? " · no carrier" : ""}
    </Badge>
  );
}

function emptyEthernet(): EthernetConfigWire {
  return { dhcp4: true };
}

function parseList(s: string): string[] {
  return s
    .split(/[,\s]+/)
    .map((x) => x.trim())
    .filter(Boolean);
}

function serialiseNs(ns?: NameserversWire): string {
  const addrs = ns?.addresses ?? [];
  return addrs.join(", ");
}

function nsFromString(s: string): NameserversWire | undefined {
  const list = parseList(s);
  if (list.length === 0) return undefined;
  return { addresses: list };
}

function deepClonePlan(p: NetplanPlan): NetplanPlan {
  return JSON.parse(JSON.stringify(p));
}

function plansEqual(a: NetplanPlan, b: NetplanPlan): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

// Format the seconds-remaining for the rollback countdown.
function fmtCountdown(secs: number): string {
  if (secs <= 0) return "0:00";
  const m = Math.floor(secs / 60);
  const s = secs % 60;
  return `${m}:${String(s).padStart(2, "0")}`;
}

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

export function AdminNetworkPage() {
  const qc = useQueryClient();

  const ifacesQuery = useQuery({
    queryKey: ["admin", "network", "interfaces"],
    queryFn: () => listNetworkInterfaces(),
    refetchInterval: 15_000,
  });

  const planQuery = useQuery({
    queryKey: ["admin", "network", "plan"],
    queryFn: () => getNetworkPlan(),
  });

  const applyStatusQuery = useQuery({
    queryKey: ["admin", "network", "apply", "status"],
    queryFn: () => getNetworkApplyStatus(),
    // Poll while an apply is in flight so the countdown updates
    // even without re-renders driven by the local timer.
    refetchInterval: 5_000,
  });

  const physicalNics = useMemo(
    () =>
      (ifacesQuery.data?.interfaces ?? []).filter(
        (n) => !n.is_loopback && n.kind === "physical",
      ),
    [ifacesQuery.data],
  );

  // ---- Editable plan draft ------------------------------------------------
  //
  // The draft is seeded from the server's `getNetworkPlan` response
  // and overlaid with one stub `EthernetConfig {}` per physical NIC
  // that isn't already in the plan, so every NIC gets a row in the
  // editor regardless of whether the operator has touched it.

  const [draft, setDraft] = useState<NetplanPlan>({});
  const [seeded, setSeeded] = useState(false);

  useEffect(() => {
    if (!planQuery.data || !ifacesQuery.data || seeded) return;
    const server = planQuery.data.plan;
    const next: NetplanPlan = deepClonePlan(server);
    next.ethernets ??= {};
    for (const nic of physicalNics) {
      if (!next.ethernets[nic.name]) {
        // Seed with a dhcp4=true placeholder so the row renders
        // pre-filled with the operator's most-common starting
        // point; persists only when they actually click Save.
        next.ethernets[nic.name] = emptyEthernet();
      }
    }
    setDraft(next);
    setSeeded(true);
  }, [planQuery.data, ifacesQuery.data, physicalNics, seeded]);

  const isDirty = useMemo(() => {
    const server = planQuery.data?.plan ?? {};
    return !plansEqual(draft, server);
  }, [draft, planQuery.data]);

  const saveMutation = useMutation({
    mutationFn: () => putNetworkPlan(draft),
    onSuccess: () => {
      toast.success("Plan saved (not yet applied)");
      void qc.invalidateQueries({ queryKey: ["admin", "network", "plan"] });
    },
    onError: (err: unknown) => {
      const msg = err instanceof ApiError ? err.message : String(err);
      toast.error(`Save failed: ${msg}`);
    },
  });

  const applyMutation = useMutation({
    mutationFn: () => applyNetworkPlan(),
    onSuccess: (res) => {
      toast.success(
        `Applied — confirm within ${fmtCountdown(
          Math.floor(
            (new Date(res.session.rollback_at).getTime() - Date.now()) / 1000,
          ),
        )} or auto-rollback`,
      );
      void qc.invalidateQueries({ queryKey: ["admin", "network"] });
    },
    onError: (err: unknown) => {
      if (err instanceof ApiError && err.status === 501) {
        toast.error(
          "OS network changes are only supported on Linux. macOS dev is read-only.",
        );
        return;
      }
      const msg = err instanceof ApiError ? err.message : String(err);
      toast.error(`Apply failed: ${msg}`);
    },
  });

  const confirmMutation = useMutation({
    mutationFn: (token: string) => confirmNetworkApply(token),
    onSuccess: () => {
      toast.success("Apply confirmed");
      void qc.invalidateQueries({ queryKey: ["admin", "network"] });
    },
    onError: (err: unknown) => {
      const msg = err instanceof ApiError ? err.message : String(err);
      toast.error(`Confirm failed: ${msg}`);
    },
  });

  const rollbackMutation = useMutation({
    mutationFn: () => rollbackNetworkApply(),
    onSuccess: () => {
      toast.success("Rolled back");
      void qc.invalidateQueries({ queryKey: ["admin", "network"] });
    },
    onError: (err: unknown) => {
      const msg = err instanceof ApiError ? err.message : String(err);
      toast.error(`Rollback failed: ${msg}`);
    },
  });

  // ---- In-flight apply banner countdown -----------------------------------

  const pending =
    applyStatusQuery.data?.session ?? planQuery.data?.apply_pending;

  return (
    <div className="space-y-6">
      <header>
        <h1 className="text-2xl font-semibold">Network</h1>
        <p className="text-sm text-muted-foreground">
          Live OS interfaces and the persisted netplan plan. Plan edits
          take effect when you click Apply. A 120-second auto-rollback
          guards against locking yourself out — re-handshake and
          confirm to keep the change.
        </p>
      </header>

      {pending ? (
        <ApplyPendingBanner
          rollbackAtIso={pending.rollback_at}
          token={pending.apply_token}
          onConfirm={() => confirmMutation.mutate(pending.apply_token)}
          onRollback={() => rollbackMutation.mutate()}
          confirming={confirmMutation.isPending}
          rolling={rollbackMutation.isPending}
        />
      ) : null}

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2 text-base">
            <NetworkIcon className="h-4 w-4 text-muted-foreground" />
            Live interfaces
          </CardTitle>
        </CardHeader>
        <CardContent className="text-sm">
          {ifacesQuery.isLoading ? (
            <Skeleton className="h-24 w-full" />
          ) : ifacesQuery.error ? (
            <p className="text-destructive">
              Failed to list interfaces: {String(ifacesQuery.error)}
            </p>
          ) : (
            <InterfacesTable interfaces={ifacesQuery.data?.interfaces ?? []} />
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">Physical NICs</CardTitle>
        </CardHeader>
        <CardContent className="space-y-4 text-sm">
          {physicalNics.length === 0 ? (
            <p className="text-muted-foreground">
              No physical NICs detected.
            </p>
          ) : (
            physicalNics.map((nic) => (
              <EthernetEditor
                key={nic.name}
                nic={nic}
                value={draft.ethernets?.[nic.name] ?? emptyEthernet()}
                onChange={(next) => {
                  setDraft((prev) => {
                    const out = deepClonePlan(prev);
                    out.ethernets = { ...(out.ethernets ?? {}), [nic.name]: next };
                    return out;
                  });
                }}
              />
            ))
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center justify-between text-base">
            <span>VLAN sub-interfaces</span>
            <VlanAddButton
              physicalNics={physicalNics}
              existing={draft.vlans ?? {}}
              onAdd={(name, cfg) => {
                setDraft((prev) => {
                  const out = deepClonePlan(prev);
                  out.vlans = { ...(out.vlans ?? {}), [name]: cfg };
                  return out;
                });
              }}
            />
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-4 text-sm">
          {Object.keys(draft.vlans ?? {}).length === 0 ? (
            <p className="text-muted-foreground">
              No VLANs in the plan. Click <kbd>+ VLAN</kbd> to add one —
              e.g. <code className="font-mono">eno1.20</code> for VLAN 20
              on <code className="font-mono">eno1</code>.
            </p>
          ) : (
            Object.entries(draft.vlans ?? {}).map(([name, cfg]) => (
              <VlanEditor
                key={name}
                name={name}
                value={cfg}
                onChange={(next) => {
                  setDraft((prev) => {
                    const out = deepClonePlan(prev);
                    out.vlans = { ...(out.vlans ?? {}), [name]: next };
                    return out;
                  });
                }}
                onRemove={() => {
                  setDraft((prev) => {
                    const out = deepClonePlan(prev);
                    if (out.vlans) {
                      const rest = { ...out.vlans };
                      delete rest[name];
                      out.vlans = rest;
                    }
                    return out;
                  });
                }}
              />
            ))
          )}
        </CardContent>
      </Card>

      <div className="sticky bottom-4 z-10 flex justify-end gap-2 rounded-md border bg-background/95 p-3 shadow-sm backdrop-blur">
        <Button
          variant="outline"
          onClick={() => {
            setSeeded(false);
            void qc.invalidateQueries({ queryKey: ["admin", "network", "plan"] });
          }}
          disabled={saveMutation.isPending}
        >
          <RotateCcw className="mr-2 h-4 w-4" />
          Discard
        </Button>
        <Button
          onClick={() => saveMutation.mutate()}
          disabled={!isDirty || saveMutation.isPending}
          data-testid="network-save"
        >
          <Save className="mr-2 h-4 w-4" />
          {saveMutation.isPending ? "Saving…" : "Save plan"}
        </Button>
        <Button
          variant="destructive"
          onClick={() => {
            if (isDirty) {
              toast.error("Save the plan first, then Apply.");
              return;
            }
            applyMutation.mutate();
          }}
          disabled={
            applyMutation.isPending ||
            !!pending ||
            isDirty
          }
          data-testid="network-apply"
        >
          <AlertTriangle className="mr-2 h-4 w-4" />
          {applyMutation.isPending ? "Applying…" : "Apply"}
        </Button>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

function InterfacesTable({ interfaces }: { interfaces: NetworkInterface[] }) {
  if (interfaces.length === 0) {
    return <p className="text-muted-foreground">No interfaces detected.</p>;
  }
  return (
    <div className="overflow-x-auto">
      <table className="w-full text-left text-xs">
        <thead className="border-b text-muted-foreground">
          <tr>
            <th className="py-2 pr-3">Name</th>
            <th className="pr-3">Kind</th>
            <th className="pr-3">State</th>
            <th className="pr-3">MAC</th>
            <th className="pr-3">Addresses</th>
            <th className="pr-3">MTU</th>
            <th className="pr-3">VLAN</th>
          </tr>
        </thead>
        <tbody>
          {interfaces.map((nic) => (
            <tr key={nic.name} className="border-b last:border-b-0">
              <td className="py-1.5 pr-3 font-mono">{nic.name}</td>
              <td className="pr-3">
                <Badge variant={kindBadgeVariant(nic.kind)}>{nic.kind}</Badge>
              </td>
              <td className="pr-3">{operstateBadge(nic)}</td>
              <td className="pr-3 font-mono text-[11px]">
                {nic.mac ?? "—"}
              </td>
              <td className="pr-3 font-mono text-[11px]">{fmtAddrs(nic)}</td>
              <td className="pr-3 font-mono text-[11px]">{nic.mtu ?? "—"}</td>
              <td className="pr-3 font-mono text-[11px]">
                {nic.parent ? `${nic.parent} · ${nic.vlan_id}` : "—"}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function EthernetEditor({
  nic,
  value,
  onChange,
}: {
  nic: NetworkInterface;
  value: EthernetConfigWire;
  onChange: (next: EthernetConfigWire) => void;
}) {
  const dhcp = value.dhcp4 === true;
  return (
    <fieldset className="space-y-3 rounded-md border p-3">
      <legend className="px-1 text-xs font-medium">
        <span className="font-mono">{nic.name}</span>{" "}
        <span className="text-muted-foreground">
          ({fmtAddrs(nic) || "no live addresses"})
        </span>
      </legend>

      <div className="flex items-center gap-2">
        <input
          id={`dhcp-${nic.name}`}
          type="checkbox"
          checked={dhcp}
          onChange={(e) =>
            onChange({
              ...value,
              dhcp4: e.target.checked,
              // Clearing static addresses when toggling to dhcp avoids the
              // "dhcp4 + addresses are mutually exclusive" validation error.
              addresses: e.target.checked ? [] : value.addresses,
            })
          }
          className="h-4 w-4"
        />
        <Label htmlFor={`dhcp-${nic.name}`} className="text-xs">
          Use DHCP (IPv4)
        </Label>
      </div>

      {!dhcp ? (
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <Field label="Static addresses (CIDR, comma-separated)">
            <Input
              placeholder="192.168.1.66/24"
              value={(value.addresses ?? []).join(", ")}
              onChange={(e) =>
                onChange({ ...value, addresses: parseList(e.target.value) })
              }
            />
          </Field>
          <Field label="Default gateway">
            <Input
              placeholder="192.168.1.1"
              value={value.gateway ?? ""}
              onChange={(e) =>
                onChange({
                  ...value,
                  gateway: e.target.value.trim() || undefined,
                })
              }
            />
          </Field>
        </div>
      ) : null}

      <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
        <Field label="DNS servers (comma-separated)">
          <Input
            placeholder="1.1.1.1, 9.9.9.9"
            value={serialiseNs(value.nameservers)}
            onChange={(e) =>
              onChange({ ...value, nameservers: nsFromString(e.target.value) })
            }
          />
        </Field>
        <Field label="MTU">
          <Input
            type="number"
            min={68}
            max={9216}
            placeholder="1500"
            value={value.mtu ?? ""}
            onChange={(e) => {
              const n = Number(e.target.value);
              onChange({
                ...value,
                mtu: Number.isFinite(n) && n > 0 ? n : undefined,
              });
            }}
          />
        </Field>
        <Field label="MAC override">
          <Input
            placeholder="aa:bb:cc:dd:ee:ff"
            value={value.macaddress ?? ""}
            onChange={(e) =>
              onChange({
                ...value,
                macaddress: e.target.value.trim() || undefined,
              })
            }
          />
        </Field>
      </div>
    </fieldset>
  );
}

function VlanEditor({
  name,
  value,
  onChange,
  onRemove,
}: {
  name: string;
  value: VlanConfigWire;
  onChange: (next: VlanConfigWire) => void;
  onRemove: () => void;
}) {
  const dhcp = value.dhcp4 === true;
  return (
    <fieldset className="space-y-3 rounded-md border p-3">
      <legend className="px-1 text-xs font-medium">
        <span className="font-mono">{name}</span>{" "}
        <span className="text-muted-foreground">
          (VLAN {value.id} on{" "}
          <span className="font-mono">{value.link}</span>)
        </span>
      </legend>

      <div className="flex items-center gap-2">
        <input
          id={`dhcp-${name}`}
          type="checkbox"
          checked={dhcp}
          onChange={(e) =>
            onChange({
              ...value,
              dhcp4: e.target.checked,
              addresses: e.target.checked ? [] : value.addresses,
            })
          }
          className="h-4 w-4"
        />
        <Label htmlFor={`dhcp-${name}`} className="text-xs">
          Use DHCP (IPv4)
        </Label>
      </div>

      {!dhcp ? (
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <Field label="Static addresses">
            <Input
              placeholder="10.20.0.5/24"
              value={(value.addresses ?? []).join(", ")}
              onChange={(e) =>
                onChange({ ...value, addresses: parseList(e.target.value) })
              }
            />
          </Field>
          <Field label="Default gateway">
            <Input
              placeholder="10.20.0.1"
              value={value.gateway ?? ""}
              onChange={(e) =>
                onChange({
                  ...value,
                  gateway: e.target.value.trim() || undefined,
                })
              }
            />
          </Field>
        </div>
      ) : null}

      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
        <Field label="DNS servers">
          <Input
            placeholder="1.1.1.1, 9.9.9.9"
            value={serialiseNs(value.nameservers)}
            onChange={(e) =>
              onChange({ ...value, nameservers: nsFromString(e.target.value) })
            }
          />
        </Field>
        <Field label="MTU">
          <Input
            type="number"
            min={68}
            max={9216}
            placeholder="1500"
            value={value.mtu ?? ""}
            onChange={(e) => {
              const n = Number(e.target.value);
              onChange({
                ...value,
                mtu: Number.isFinite(n) && n > 0 ? n : undefined,
              });
            }}
          />
        </Field>
      </div>

      <div className="flex justify-end">
        <Button variant="ghost" size="sm" onClick={onRemove}>
          <Trash2 className="mr-2 h-3 w-3" />
          Remove
        </Button>
      </div>
    </fieldset>
  );
}

function VlanAddButton({
  physicalNics,
  existing,
  onAdd,
}: {
  physicalNics: NetworkInterface[];
  existing: Record<string, VlanConfigWire>;
  onAdd: (name: string, cfg: VlanConfigWire) => void;
}) {
  const [open, setOpen] = useState(false);
  const [link, setLink] = useState("");
  const [id, setId] = useState<number>(20);

  useEffect(() => {
    if (open && link === "" && physicalNics.length > 0) {
      const first = physicalNics[0];
      if (first) setLink(first.name);
    }
  }, [open, link, physicalNics]);

  if (!open) {
    return (
      <Button
        variant="outline"
        size="sm"
        onClick={() => setOpen(true)}
        disabled={physicalNics.length === 0}
      >
        <Plus className="mr-2 h-3 w-3" />
        VLAN
      </Button>
    );
  }

  return (
    <div className="flex items-center gap-2">
      <select
        className="rounded-md border bg-background px-2 py-1 text-xs"
        value={link}
        onChange={(e) => setLink(e.target.value)}
      >
        {physicalNics.map((n) => (
          <option key={n.name} value={n.name}>
            {n.name}
          </option>
        ))}
      </select>
      <span className="text-xs text-muted-foreground">·</span>
      <Input
        type="number"
        min={1}
        max={4094}
        value={id}
        onChange={(e) => setId(Number(e.target.value))}
        className="h-7 w-20 text-xs"
      />
      <Button
        size="sm"
        onClick={() => {
          if (!link || id < 1 || id > 4094) {
            toast.error("VLAN id must be 1–4094 on a physical NIC.");
            return;
          }
          const name = `${link}.${id}`;
          if (existing[name]) {
            toast.error(`${name} is already in the plan.`);
            return;
          }
          onAdd(name, { id, link, dhcp4: true });
          setOpen(false);
          setLink("");
          setId(20);
        }}
      >
        Add
      </Button>
      <Button variant="ghost" size="sm" onClick={() => setOpen(false)}>
        Cancel
      </Button>
    </div>
  );
}

function Field({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex flex-col gap-1">
      <Label className="text-xs">{label}</Label>
      {children}
    </div>
  );
}

function ApplyPendingBanner({
  rollbackAtIso,
  token,
  onConfirm,
  onRollback,
  confirming,
  rolling,
}: {
  rollbackAtIso: string;
  token: string;
  onConfirm: () => void;
  onRollback: () => void;
  confirming: boolean;
  rolling: boolean;
}) {
  const [now, setNow] = useState(() => Date.now());
  const timerRef = useRef<number | null>(null);
  useEffect(() => {
    timerRef.current = window.setInterval(() => setNow(Date.now()), 1000);
    return () => {
      if (timerRef.current !== null) window.clearInterval(timerRef.current);
    };
  }, []);
  const deadline = new Date(rollbackAtIso).getTime();
  const remaining = Math.max(0, Math.floor((deadline - now) / 1000));
  const urgent = remaining < 30;
  return (
    <div
      className={cn(
        "flex flex-col gap-3 rounded-md border p-4 sm:flex-row sm:items-center sm:justify-between",
        urgent
          ? "border-destructive/60 bg-destructive/10"
          : "border-warning/60 bg-warning/10",
      )}
      data-testid="network-apply-pending"
    >
      <div className="flex items-start gap-3">
        <AlertTriangle
          className={cn(
            "mt-0.5 h-5 w-5",
            urgent ? "text-destructive" : "text-warning",
          )}
        />
        <div>
          <p className="text-sm font-medium">
            Network change pending — auto-rollback in{" "}
            <span className="font-mono">{fmtCountdown(remaining)}</span>
          </p>
          <p className="mt-0.5 text-xs text-muted-foreground">
            Reload the page over the new bind/VLAN to confirm you're still
            reachable, then click <strong>Confirm</strong>. Otherwise the
            engine restores the previous netplan and re-applies.
          </p>
          <p className="mt-1 text-[10px] text-muted-foreground font-mono">
            token: {token}
          </p>
        </div>
      </div>
      <div className="flex gap-2 sm:flex-col">
        <Button
          size="sm"
          onClick={onConfirm}
          disabled={confirming || remaining === 0}
        >
          <CheckCircle2 className="mr-2 h-4 w-4" />
          {confirming ? "Confirming…" : "Confirm"}
        </Button>
        <Button
          size="sm"
          variant="outline"
          onClick={onRollback}
          disabled={rolling}
        >
          <Undo2 className="mr-2 h-4 w-4" />
          {rolling ? "Rolling back…" : "Roll back now"}
        </Button>
      </div>
    </div>
  );
}
