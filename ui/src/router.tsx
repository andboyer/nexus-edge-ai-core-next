// TanStack Router \u2014 code-defined route tree. We pick code-defined
// (over file-based) because the route tree is small and the auth
// gate logic is easier to reason about when it's all in one file.
//
// Auth gating: the `_app` root route's `beforeLoad` redirects to
// /login when no session is present. Pages under `_app` can assume
// `useSession()` is non-null.

import {
  Outlet,
  createRootRoute,
  createRoute,
  createRouter,
  redirect,
} from "@tanstack/react-router";

import { AppShell } from "@/components/layout/AppShell";
import { RouteErrorBoundary } from "@/components/RouteErrorBoundary";
import { AdminAuditPage } from "@/pages/admin-audit";
import { AdminAuthPage } from "@/pages/admin-auth";
import { AdminDiagnosticsPage } from "@/pages/admin-diagnostics";
import { AdminServerPage } from "@/pages/admin-server";
import { AdminUsersPage } from "@/pages/admin-users";
import { BackendsPage } from "@/pages/backends";
import { CamerasPage } from "@/pages/cameras";
import { DashboardPage } from "@/pages/dashboard";
import { DeliveryPage } from "@/pages/delivery";
import { EventsPage } from "@/pages/events";
import { LoginPage } from "@/pages/login";
import { RulesPage } from "@/pages/rules";
import { StoragePage } from "@/pages/storage";
import { SystemPage } from "@/pages/system";
import { TimelinePage } from "@/pages/timeline";
import { ViewerPage } from "@/pages/viewer";
import { VisualPromptsPage } from "@/pages/visual-prompts";

// ---------------------------------------------------------------------------
// Root \u2014 no chrome. Children render their own layout.
// ---------------------------------------------------------------------------

const rootRoute = createRootRoute({
  component: () => <Outlet />,
  errorComponent: RouteErrorBoundary,
});

// ---------------------------------------------------------------------------
// Public routes (login, OIDC callback eventually).
// ---------------------------------------------------------------------------

const loginRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/login",
  component: LoginPage,
});

// ---------------------------------------------------------------------------
// Authenticated app shell. beforeLoad reads localStorage directly so the
// gate doesn't depend on React state \u2014 router resolves before the
// component tree mounts.
// ---------------------------------------------------------------------------

const appRoute = createRoute({
  getParentRoute: () => rootRoute,
  id: "_app",
  beforeLoad: ({ location }) => {
    const raw = localStorage.getItem("nexus_session");
    if (!raw) {
      throw redirect({
        to: "/login",
        search: { from: location.pathname },
      });
    }
  },
  component: AppShell,
  errorComponent: RouteErrorBoundary,
});

// Root "/" \u2192 dashboard.
const indexRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/",
  beforeLoad: () => {
    throw redirect({ to: "/dashboard" });
  },
  component: () => null,
});

// Authenticated leaf routes — all phases now wired.

const dashboardRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/dashboard",
  component: DashboardPage,
});

const systemRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/system",
  component: SystemPage,
});

const viewerRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/viewer",
  component: ViewerPage,
});

const eventsRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/events",
  component: EventsPage,
});

const timelineRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/timeline",
  component: TimelinePage,
});

const camerasRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/cameras",
  component: CamerasPage,
});

// Deep-link to a specific camera. The page itself reads
// `$id` via `useParams` and auto-opens the editor sheet. List
// view remains the index above.
const cameraDetailRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/cameras/$id",
  component: CamerasPage,
});

const rulesRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/rules",
  component: RulesPage,
});

const ruleDetailRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/rules/$id",
  component: RulesPage,
});

const visualPromptsRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/visual-prompts",
  component: VisualPromptsPage,
});

const storageRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/storage",
  component: StoragePage,
});

const deliveryRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/delivery",
  component: DeliveryPage,
});

const backendsRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/backends",
  component: BackendsPage,
});

const adminUsersRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/admin/users",
  component: AdminUsersPage,
});

const adminAuditRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/admin/audit",
  component: AdminAuditPage,
});

const adminServerRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/admin/server",
  component: AdminServerPage,
});

const adminAuthRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/admin/auth",
  component: AdminAuthPage,
});

const adminDiagnosticsRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/admin/diagnostics",
  component: AdminDiagnosticsPage,
});

// ---------------------------------------------------------------------------
// Compose tree + export router.
// ---------------------------------------------------------------------------

const routeTree = rootRoute.addChildren([
  loginRoute,
  appRoute.addChildren([
    indexRoute,
    dashboardRoute,
    systemRoute,
    viewerRoute,
    eventsRoute,
    timelineRoute,
    camerasRoute,
    cameraDetailRoute,
    rulesRoute,
    ruleDetailRoute,
    visualPromptsRoute,
    storageRoute,
    deliveryRoute,
    backendsRoute,
    adminUsersRoute,
    adminAuditRoute,
    adminServerRoute,
    adminAuthRoute,
    adminDiagnosticsRoute,
  ]),
]);

export const router = createRouter({
  routeTree,
  defaultPreload: "intent",
});

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
