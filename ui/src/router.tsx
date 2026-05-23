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
import { AdminNetworkPage } from "@/pages/admin-network";
import { AdminServerPage } from "@/pages/admin-server";
import { AdminUsersPage } from "@/pages/admin-users";
import { BackendsPage } from "@/pages/backends";
import { CamerasPage } from "@/pages/cameras";
import { DashboardPage } from "@/pages/dashboard";
import { DeliveryPage } from "@/pages/delivery";
import { EventsPage } from "@/pages/events";
import { LoginPage } from "@/pages/login";
import { RulesPage } from "@/pages/rules";
import { SetupPage } from "@/pages/setup";
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
// Setup wizard \u2014 authenticated but outside the AppShell chrome and
// outside the `setup_complete` gate (otherwise it would redirect to
// itself). Once the operator clicks Finish, the engine flips the
// `engine_runtime_settings.setup_complete` latch and subsequent loads
// of /setup bounce to /dashboard.
// ---------------------------------------------------------------------------

const setupRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/setup",
  beforeLoad: async () => {
    const raw = localStorage.getItem("nexus_session");
    if (!raw) {
      throw redirect({ to: "/login" });
    }
    // If setup was already completed, send the operator to the dashboard.
    const done = await fetchSetupComplete();
    if (done === true) {
      throw redirect({ to: "/dashboard" });
    }
  },
  component: SetupPage,
});

// ---------------------------------------------------------------------------
// Authenticated app shell. beforeLoad reads localStorage directly so the
// gate doesn't depend on React state \u2014 router resolves before the
// component tree mounts.
// ---------------------------------------------------------------------------

const appRoute = createRoute({
  getParentRoute: () => rootRoute,
  id: "_app",
  beforeLoad: async ({ location }) => {
    const raw = localStorage.getItem("nexus_session");
    if (!raw) {
      throw redirect({
        to: "/login",
        search: { from: location.pathname },
      });
    }
    // First-boot gate. If the engine hasn't been marked
    // `setup_complete`, bounce the operator into the wizard. We
    // treat "could not reach the engine" as "let the page mount
    // and show its own error UI" \u2014 don't trap the user in a
    // redirect loop when the API is down.
    const done = await fetchSetupComplete();
    if (done === false) {
      throw redirect({ to: "/setup" });
    }
  },
  component: AppShell,
  errorComponent: RouteErrorBoundary,
});

// Reads /api/v1/setup/status with the bearer from localStorage. Returns
// `true` (complete), `false` (incomplete), or `null` (unknown \u2014 transient
// failure, missing token, etc). Callers MUST distinguish null from false
// to avoid a redirect loop when the engine is briefly unreachable.
async function fetchSetupComplete(): Promise<boolean | null> {
  try {
    const raw = localStorage.getItem("nexus_session");
    if (!raw) return null;
    const sess = JSON.parse(raw) as { access_token?: string };
    if (!sess.access_token) return null;
    const r = await fetch("/api/v1/setup/status", {
      headers: {
        Accept: "application/json",
        Authorization: `Bearer ${sess.access_token}`,
      },
    });
    if (!r.ok) return null;
    const body = (await r.json()) as { setup_complete?: boolean };
    return body.setup_complete === true;
  } catch {
    return null;
  }
}

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

const adminNetworkRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/admin/network",
  component: AdminNetworkPage,
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
  setupRoute,
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
    adminNetworkRoute,
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
