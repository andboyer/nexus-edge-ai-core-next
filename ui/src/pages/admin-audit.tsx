// Admin Audit log page.

import { useQuery } from "@tanstack/react-query";
import {
  AlertCircle,
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  Clock,
  ShieldOff,
  X,
} from "lucide-react";
import { useMemo, useState } from "react";

import { listAudit } from "@/api/admin";
import type { AuditOutcome, AuditRowOut, ListAuditQuery } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { formatAgo } from "@/lib/format";

const PAGE_SIZE = 50;

export function AdminAuditPage() {
  const [filters, setFilters] = useState<ListAuditQuery>({});
  const [offset, setOffset] = useState(0);
  const [expanded, setExpanded] = useState<number | null>(null);

  const auditQuery = useQuery({
    queryKey: ["admin", "audit", filters, offset],
    queryFn: () =>
      listAudit({ ...filters, limit: PAGE_SIZE, offset }),
    placeholderData: (prev) => prev,
  });

  const rows = auditQuery.data?.rows ?? [];

  const setFilter = <K extends keyof ListAuditQuery>(
    k: K,
    v: ListAuditQuery[K] | undefined,
  ) => {
    setFilters((f) => {
      const next = { ...f };
      if (v === undefined || v === "" || v === null) {
        delete next[k];
      } else {
        next[k] = v;
      }
      return next;
    });
    setOffset(0);
    setExpanded(null);
  };

  return (
    <div className="space-y-6">
      <header>
        <h1 className="text-2xl font-semibold">Audit log</h1>
        <p className="text-sm text-muted-foreground">
          Every privileged mutation. Deletes preserve actor identity via
          denormalised label, so history survives user deletion.
        </p>
      </header>

      <Card>
        <CardContent className="space-y-3 p-4">
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
            <FilterInput
              label="Actor"
              value={filters.actor_id ?? ""}
              onChange={(v) => setFilter("actor_id", v)}
              placeholder="user id"
            />
            <FilterInput
              label="Action"
              value={filters.action ?? ""}
              onChange={(v) => setFilter("action", v)}
              placeholder="camera.upsert"
            />
            <FilterInput
              label="Resource kind"
              value={filters.resource_kind ?? ""}
              onChange={(v) => setFilter("resource_kind", v)}
              placeholder="camera"
            />
            <FilterInput
              label="Resource id"
              value={filters.resource_id ?? ""}
              onChange={(v) => setFilter("resource_id", v)}
              placeholder="cam-front-door"
            />
            <div className="space-y-1.5">
              <Label className="text-xs text-muted-foreground">Outcome</Label>
              <select
                className="h-9 w-full rounded-md border border-input bg-transparent px-2 text-sm"
                value={filters.outcome ?? ""}
                onChange={(e) =>
                  setFilter(
                    "outcome",
                    e.target.value === ""
                      ? undefined
                      : (e.target.value as AuditOutcome),
                  )
                }
              >
                <option value="">all</option>
                <option value="success">success</option>
                <option value="failure">failure</option>
                <option value="denied">denied</option>
              </select>
            </div>
            <FilterInput
              label="Since (RFC3339)"
              value={filters.since ?? ""}
              onChange={(v) => setFilter("since", v)}
              placeholder="2026-05-01T00:00:00Z"
            />
            <FilterInput
              label="Until (RFC3339)"
              value={filters.until ?? ""}
              onChange={(v) => setFilter("until", v)}
              placeholder="2026-05-20T00:00:00Z"
            />
            <div className="flex items-end">
              <Button
                variant="outline"
                size="sm"
                onClick={() => {
                  setFilters({});
                  setOffset(0);
                  setExpanded(null);
                }}
              >
                <X className="mr-1 h-3 w-3" />
                Clear filters
              </Button>
            </div>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardContent className="p-0">
          {auditQuery.isLoading ? (
            <div className="space-y-2 p-4">
              {[0, 1, 2, 3].map((i) => (
                <Skeleton key={i} className="h-10 w-full" />
              ))}
            </div>
          ) : rows.length === 0 ? (
            <p className="py-12 text-center text-sm text-muted-foreground">
              No audit entries match.
            </p>
          ) : (
            <div className="overflow-x-auto">
              <table className="w-full text-sm">
                <thead className="bg-muted/30 text-xs uppercase text-muted-foreground">
                  <tr>
                    <th className="w-6 px-2 py-2"></th>
                    <th className="px-2 py-2 text-left">When</th>
                    <th className="px-2 py-2 text-left">Actor</th>
                    <th className="px-2 py-2 text-left">Action</th>
                    <th className="px-2 py-2 text-left">Resource</th>
                    <th className="px-2 py-2 text-left">Outcome</th>
                  </tr>
                </thead>
                <tbody>
                  {rows.map((r) => (
                    <AuditRowItem
                      key={r.id}
                      row={r}
                      expanded={expanded === r.id}
                      onToggle={() =>
                        setExpanded(expanded === r.id ? null : r.id)
                      }
                    />
                  ))}
                </tbody>
              </table>
            </div>
          )}
          <div className="flex items-center justify-between border-t border-border/40 px-4 py-3 text-sm text-muted-foreground">
            <span>
              Showing {offset + 1}–{offset + rows.length}
            </span>
            <div className="flex gap-2">
              <Button
                size="sm"
                variant="outline"
                disabled={offset === 0}
                onClick={() => {
                  setOffset(Math.max(0, offset - PAGE_SIZE));
                  setExpanded(null);
                }}
              >
                Previous
              </Button>
              <Button
                size="sm"
                variant="outline"
                disabled={rows.length < PAGE_SIZE}
                onClick={() => {
                  setOffset(offset + PAGE_SIZE);
                  setExpanded(null);
                }}
              >
                Next
              </Button>
            </div>
          </div>
        </CardContent>
      </Card>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Row + diff.
// ---------------------------------------------------------------------------

function AuditRowItem({
  row,
  expanded,
  onToggle,
}: {
  row: AuditRowOut;
  expanded: boolean;
  onToggle: () => void;
}) {
  const before = useMemo(() => parseJson(row.before_json), [row.before_json]);
  const after = useMemo(() => parseJson(row.after_json), [row.after_json]);
  const hasDiff = before !== null || after !== null;

  return (
    <>
      <tr
        className="cursor-pointer border-t border-border/40 hover:bg-muted/20"
        onClick={onToggle}
      >
        <td className="px-2 py-2 text-muted-foreground">
          {hasDiff ? (
            expanded ? (
              <ChevronDown className="h-4 w-4" />
            ) : (
              <ChevronRight className="h-4 w-4" />
            )
          ) : null}
        </td>
        <td className="px-2 py-2 text-xs">
          <div className="flex items-center gap-1.5">
            <Clock className="h-3 w-3 text-muted-foreground" />
            {formatAgo(row.created_at)}
          </div>
          <div className="font-mono text-[10px] text-muted-foreground">
            {row.created_at.replace("T", " ").slice(0, 19)}
          </div>
        </td>
        <td className="px-2 py-2 text-xs">
          <div className="font-medium">{row.actor_label}</div>
          <div className="text-muted-foreground">{row.actor_kind}</div>
        </td>
        <td className="px-2 py-2">
          <code className="font-mono text-xs">{row.action}</code>
        </td>
        <td className="px-2 py-2 text-xs">
          {row.resource_kind ? (
            <>
              <span className="text-muted-foreground">
                {row.resource_kind}
              </span>
              {row.resource_id ? (
                <>
                  {" "}
                  <code className="font-mono">{row.resource_id}</code>
                </>
              ) : null}
            </>
          ) : (
            <span className="text-muted-foreground">—</span>
          )}
        </td>
        <td className="px-2 py-2">
          <OutcomeBadge outcome={row.outcome} />
        </td>
      </tr>
      {expanded && hasDiff ? (
        <tr className="border-t border-border/40 bg-muted/10">
          <td colSpan={6} className="p-3">
            <DiffPanel
              before={before}
              after={after}
              ip={row.ip}
              userAgent={row.user_agent}
            />
          </td>
        </tr>
      ) : null}
    </>
  );
}

function DiffPanel({
  before,
  after,
  ip,
  userAgent,
}: {
  before: unknown;
  after: unknown;
  ip: string | null;
  userAgent: string | null;
}) {
  return (
    <div className="space-y-3 text-xs">
      <div className="grid grid-cols-1 gap-3 lg:grid-cols-2">
        <DiffPane label="Before" value={before} />
        <DiffPane label="After" value={after} />
      </div>
      {(ip || userAgent) ? (
        <div className="flex flex-wrap gap-3 text-muted-foreground">
          {ip ? (
            <span>
              ip <code className="font-mono">{ip}</code>
            </span>
          ) : null}
          {userAgent ? (
            <span className="truncate">
              ua <code className="font-mono">{userAgent}</code>
            </span>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

function DiffPane({ label, value }: { label: string; value: unknown }) {
  return (
    <div>
      <p className="mb-1 text-muted-foreground">{label}</p>
      <pre className="max-h-72 overflow-auto rounded-md border border-border/40 bg-background p-2 font-mono">
        {value === null
          ? <span className="text-muted-foreground">—</span>
          : JSON.stringify(value, null, 2)}
      </pre>
    </div>
  );
}

function OutcomeBadge({ outcome }: { outcome: AuditOutcome }) {
  if (outcome === "success") {
    return (
      <Badge variant="success">
        <CheckCircle2 className="mr-1 h-3 w-3" />
        success
      </Badge>
    );
  }
  if (outcome === "denied") {
    return (
      <Badge variant="warning">
        <ShieldOff className="mr-1 h-3 w-3" />
        denied
      </Badge>
    );
  }
  return (
    <Badge variant="destructive">
      <AlertCircle className="mr-1 h-3 w-3" />
      failure
    </Badge>
  );
}

function FilterInput({
  label,
  value,
  onChange,
  placeholder,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
}) {
  return (
    <div className="space-y-1.5">
      <Label className="text-xs text-muted-foreground">{label}</Label>
      <Input
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        className="h-9"
      />
    </div>
  );
}

function parseJson(s: string | null): unknown {
  if (!s) return null;
  try {
    return JSON.parse(s);
  } catch {
    return s;
  }
}
