// Sidebar navigation. Items grouped by section. Admin-only items
// hide for non-admins (defensive UX; the engine also rejects them).

import { Link, useRouterState } from "@tanstack/react-router";
import {
  Activity,
  AlertTriangle,
  Camera,
  Cog,
  Database,
  Eye,
  FileClock,
  Gauge,
  Layers,
  ListChecks,
  ScrollText,
  ShieldCheck,
  Sparkles,
  Truck,
  UserCog,
  Wrench,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";

import { useIsAdmin } from "@/lib/auth";
import { cn } from "@/lib/utils";

interface NavItem {
  to: string;
  label: string;
  icon: LucideIcon;
  adminOnly?: boolean;
}

interface NavSection {
  label: string;
  items: NavItem[];
}

const SECTIONS: NavSection[] = [
  {
    label: "Overview",
    items: [
      { to: "/dashboard", label: "Dashboard", icon: Gauge },
      { to: "/system", label: "System", icon: Activity },
    ],
  },
  {
    label: "Operations",
    items: [
      { to: "/viewer", label: "Viewer", icon: Eye },
      { to: "/events", label: "Events", icon: AlertTriangle },
      { to: "/timeline", label: "Timeline", icon: FileClock },
    ],
  },
  {
    label: "Configuration",
    items: [
      { to: "/cameras", label: "Cameras", icon: Camera },
      { to: "/rules", label: "Rules", icon: ListChecks },
      { to: "/visual-prompts", label: "Visual Prompts", icon: Sparkles, adminOnly: true },
    ],
  },
  {
    label: "System",
    items: [
      { to: "/storage", label: "Storage", icon: Database },
      { to: "/delivery", label: "Alert Delivery", icon: Truck },
      { to: "/backends", label: "Backends", icon: Layers },
    ],
  },
  {
    label: "Admin",
    items: [
      { to: "/admin/users", label: "Users", icon: UserCog, adminOnly: true },
      { to: "/admin/audit", label: "Audit Log", icon: ScrollText, adminOnly: true },
      { to: "/admin/server", label: "Server Settings", icon: Cog, adminOnly: true },
      { to: "/admin/auth", label: "Auth Config", icon: ShieldCheck, adminOnly: true },
      { to: "/admin/diagnostics", label: "Diagnostics", icon: Wrench, adminOnly: true },
    ],
  },
];

export function Sidebar() {
  const isAdmin = useIsAdmin();
  const pathname = useRouterState({ select: (s) => s.location.pathname });

  return (
    <aside className="w-60 shrink-0 border-r border-border bg-card/40">
      <nav className="flex h-full flex-col gap-6 overflow-y-auto p-3">
        {SECTIONS.map((section) => {
          const visible = section.items.filter((i) => !i.adminOnly || isAdmin);
          if (visible.length === 0) return null;
          return (
            <div key={section.label}>
              <div className="mb-1 px-2 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
                {section.label}
              </div>
              <ul className="space-y-0.5">
                {visible.map((item) => {
                  const Icon = item.icon;
                  const active =
                    pathname === item.to || pathname.startsWith(item.to + "/");
                  return (
                    <li key={item.to}>
                      <Link
                        to={item.to}
                        className={cn(
                          "flex items-center gap-2 rounded-md px-2 py-1.5 text-sm transition-colors",
                          active
                            ? "bg-primary/10 text-primary"
                            : "text-foreground/80 hover:bg-secondary hover:text-foreground",
                        )}
                      >
                        <Icon className="h-4 w-4" />
                        {item.label}
                      </Link>
                    </li>
                  );
                })}
              </ul>
            </div>
          );
        })}
      </nav>
    </aside>
  );
}
