// Route-level error boundary. TanStack Router calls this when a route's
// loader or component throws. Keep it self-contained — it shouldn't depend on
// any data the failing route was supposed to load.

import { useRouter } from "@tanstack/react-router";
import { AlertTriangle, RefreshCcw } from "lucide-react";

import { Button } from "@/components/ui/button";

export function RouteErrorBoundary({ error }: { error: Error }) {
  const router = useRouter();
  return (
    <div className="flex min-h-[60vh] items-center justify-center p-6">
      <div className="w-full max-w-lg rounded-lg border border-destructive/40 bg-card p-6 shadow-lg">
        <div className="flex items-start gap-3">
          <div className="rounded-md border border-destructive/40 bg-destructive/10 p-2">
            <AlertTriangle className="h-5 w-5 text-destructive" />
          </div>
          <div className="min-w-0 flex-1">
            <h2 className="text-lg font-semibold">Something went wrong</h2>
            <p className="mt-1 text-sm text-muted-foreground">
              The page hit an unexpected error. The engine API may have
              returned an unexpected shape, or this build is out of sync with
              the engine version.
            </p>
            <pre className="mt-3 max-h-40 overflow-auto rounded-md border border-border/40 bg-muted/30 p-2 font-mono text-xs">
              {String(error?.message ?? error)}
            </pre>
            <div className="mt-4 flex flex-wrap gap-2">
              <Button
                size="sm"
                onClick={() => router.invalidate()}
              >
                <RefreshCcw className="mr-2 h-4 w-4" />
                Try again
              </Button>
              <Button
                size="sm"
                variant="outline"
                onClick={() => {
                  window.location.href = "/admin/diagnostics";
                }}
              >
                Report issue
              </Button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
