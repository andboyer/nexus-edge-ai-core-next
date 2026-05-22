import { cn } from "@/lib/utils";

/** Pulsing skeleton block. Replace with real content once loaded. */
export function Skeleton({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      className={cn("animate-pulse rounded-md bg-muted/40", className)}
      {...props}
    />
  );
}
