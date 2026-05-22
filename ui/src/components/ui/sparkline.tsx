import { useMemo } from "react";

import { cn } from "@/lib/utils";

interface SparklineProps {
  values: number[];
  /** Domain max; defaults to max(values) (or 100 if all zero). */
  max?: number;
  height?: number;
  className?: string;
  /** Stroke color via Tailwind class; defaults to text-primary. */
  strokeClassName?: string;
}

/**
 * Tiny inline-SVG sparkline. Renders a smooth-ish polyline of the
 * last N samples across the full width of its container, with a
 * subtle fill below. No chart library needed.
 */
export function Sparkline({
  values,
  max,
  height = 48,
  className,
  strokeClassName = "text-primary",
}: SparklineProps) {
  const { points, area } = useMemo(() => {
    if (values.length === 0) return { points: "", area: "" };
    const ymax = max ?? Math.max(...values, 1);
    const w = 100; // viewBox width — scaled to fill via preserveAspectRatio="none"
    const h = 100; // viewBox height
    const step = values.length > 1 ? w / (values.length - 1) : 0;
    const coords = values.map((v, i) => {
      const x = i * step;
      const y = h - (Math.max(0, Math.min(v, ymax)) / ymax) * h;
      return [x, y] as const;
    });
    const pts = coords.map(([x, y]) => `${x.toFixed(2)},${y.toFixed(2)}`).join(" ");
    const first = coords[0];
    const last = coords[coords.length - 1];
    const fill = first && last ? `${first[0].toFixed(2)},${h} ${pts} ${last[0].toFixed(2)},${h}` : "";
    return { points: pts, area: fill };
  }, [values, max]);

  if (values.length === 0) {
    return <div className={cn("h-12 w-full", className)} style={{ height }} />;
  }

  return (
    <svg
      viewBox="0 0 100 100"
      preserveAspectRatio="none"
      width="100%"
      height={height}
      className={cn("overflow-visible", strokeClassName, className)}
    >
      <polygon points={area} fill="currentColor" opacity={0.12} />
      <polyline
        points={points}
        fill="none"
        stroke="currentColor"
        strokeWidth={2}
        strokeLinecap="round"
        strokeLinejoin="round"
        vectorEffect="non-scaling-stroke"
      />
    </svg>
  );
}
