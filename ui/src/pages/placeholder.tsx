// Placeholder pages \u2014 each route renders a header + "coming soon" card.
// Subsequent phases replace these with real implementations.

import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";

interface PlaceholderProps {
  title: string;
  description: string;
  phase: string;
}

export function PageHeader({ title, description }: { title: string; description?: string }) {
  return (
    <div className="border-b border-border bg-card/30 px-6 py-4">
      <h1 className="text-xl font-semibold tracking-tight">{title}</h1>
      {description ? (
        <p className="mt-1 text-sm text-muted-foreground">{description}</p>
      ) : null}
    </div>
  );
}

export function Placeholder({ title, description, phase }: PlaceholderProps) {
  return (
    <>
      <PageHeader title={title} description={description} />
      <div className="p-6">
        <Card>
          <CardHeader>
            <CardTitle className="text-base">Coming in {phase}</CardTitle>
            <CardDescription>
              This page is part of the UI rewrite. The engine endpoints it
              consumes already work; the React implementation lands shortly.
            </CardDescription>
          </CardHeader>
          <CardContent className="text-sm text-muted-foreground">
            See <code className="rounded bg-secondary px-1 py-0.5 text-xs">/memories/session/plan.md</code>{" "}
            for the full sequencing.
          </CardContent>
        </Card>
      </div>
    </>
  );
}
