import { cn } from "@/lib/utils";

interface ProgressProps {
  value: number; // 0..100
  className?: string;
  /** Tailwind background class for the fill. Defaults to bg-primary. */
  fillClassName?: string;
}

/** Simple horizontal progress bar — no Radix dep for a 5-line primitive. */
export function Progress({ value, className, fillClassName = "bg-primary" }: ProgressProps) {
  const v = Math.max(0, Math.min(100, value));
  return (
    <div
      className={cn(
        "relative h-2 w-full overflow-hidden rounded-full bg-muted/40",
        className,
      )}
      role="progressbar"
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(v)}
    >
      <div
        className={cn("h-full transition-all", fillClassName)}
        style={{ width: `${v}%` }}
      />
    </div>
  );
}
