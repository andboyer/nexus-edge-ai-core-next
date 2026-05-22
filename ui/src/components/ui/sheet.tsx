// Lightweight right-side drawer primitive. No Radix dep — Phase 4 only
// needs open/close, escape key, click-outside, header, scrollable body.
//
// Usage:
//   <Sheet open={open} onClose={() => setOpen(false)} title="...">
//     <SheetSection>...</SheetSection>
//   </Sheet>

import { X } from "lucide-react";
import { useEffect } from "react";
import type { ReactNode } from "react";

export function Sheet({
  open,
  onClose,
  title,
  description,
  footer,
  children,
  width = "max-w-2xl",
}: {
  open: boolean;
  onClose: () => void;
  title: ReactNode;
  description?: ReactNode;
  footer?: ReactNode;
  children: ReactNode;
  width?: string;
}) {
  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-40 flex justify-end"
      role="dialog"
      aria-modal="true"
    >
      <div
        className="flex-1 bg-background/60 backdrop-blur-sm"
        onClick={onClose}
        aria-hidden
      />
      <aside
        className={`flex w-full ${width} flex-col overflow-hidden border-l border-border bg-background shadow-2xl`}
      >
        <header className="flex items-start justify-between gap-2 border-b border-border px-5 py-4">
          <div className="min-w-0">
            <h2 className="text-lg font-semibold">{title}</h2>
            {description ? (
              <p className="mt-1 text-xs text-muted-foreground">{description}</p>
            ) : null}
          </div>
          <button
            type="button"
            className="rounded-md p-1.5 hover:bg-muted"
            onClick={onClose}
            aria-label="Close"
          >
            <X className="h-4 w-4" />
          </button>
        </header>
        <div className="flex-1 overflow-y-auto">{children}</div>
        {footer ? (
          <footer className="flex justify-end gap-2 border-t border-border px-5 py-3">
            {footer}
          </footer>
        ) : null}
      </aside>
    </div>
  );
}

export function SheetSection({
  title,
  children,
  description,
}: {
  title?: ReactNode;
  description?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="space-y-3 border-b border-border/40 px-5 py-4 last:border-b-0">
      {title ? (
        <div>
          <h3 className="text-sm font-semibold">{title}</h3>
          {description ? (
            <p className="text-xs text-muted-foreground">{description}</p>
          ) : null}
        </div>
      ) : null}
      {children}
    </section>
  );
}
