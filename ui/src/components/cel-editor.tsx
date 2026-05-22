// CEL expression editor backed by CodeMirror 6.
//
// CEL (Common Expression Language) has no official Lezer grammar, but
// its surface syntax is a strict subset of JavaScript's expressions
// (string/number literals, identifier chains, ternaries, arithmetic,
// comparison, &&/||, dotted member access, function-call form). The
// `javascript` mode therefore highlights real CEL very faithfully —
// keywords like `true`/`false`/`null`, operators, and strings all
// colour correctly. Server-side `/rules/validate` remains the source
// of truth for actual semantic checking; it's wired in here as a
// debounced linter so the user gets red squigglies as they type.
//
// Custom completion source ([`makeCelCompletionSource`]) handles:
//   - Top-level variables (object/track/camera/frame/now) + stdlib fns
//   - Dotted property completion (object.| -> label/confidence/box/...)
//   - Bracket-key completion (object.attributes[| -> 'motion.*' / 'group.*')
//   - Enum-value completion after `== '` for known string fields
//   - Snippet templates for common predicates
//
// Theming pins to `oneDark` because the rest of the app is dark-only
// (see `<Toaster theme="dark" />` in `main.tsx` and the dark Tailwind
// palette). If the app ever sprouts a light mode this is the one spot
// to thread the active theme through.

import { autocompletion } from "@codemirror/autocomplete";
import { javascript } from "@codemirror/lang-javascript";
import { type Diagnostic, linter } from "@codemirror/lint";
import { EditorView } from "@codemirror/view";
import { oneDark } from "@codemirror/theme-one-dark";
import { useQuery } from "@tanstack/react-query";
import CodeMirror from "@uiw/react-codemirror";
import { useMemo } from "react";

import { getRulesSchema, validateRuleCel } from "@/api/config";
import {
  type CelExternalCatalogs,
  makeCelCompletionSource,
} from "@/lib/cel-completion";
import { cn } from "@/lib/utils";

export interface CelEditorProps {
  value: string;
  onChange: (next: string) => void;
  minHeight?: string;
  className?: string;
  readOnly?: boolean;
  /** Optional id forwarded to the underlying contenteditable host. */
  id?: string;
  /** Disable lint-on-type. Defaults to `true`. */
  lint?: boolean;
}

// Surface tweaks so the editor blends with the shadcn `<Input>` /
// `<Textarea>` chrome — same border radius, same focus ring colour.
const surfaceTheme = EditorView.theme({
  "&": {
    fontSize: "0.875rem",
    borderRadius: "calc(var(--radius) - 2px)",
    border: "1px solid hsl(var(--input))",
    backgroundColor: "transparent",
    overflow: "hidden",
  },
  "&.cm-focused": {
    outline: "none",
    borderColor: "hsl(var(--ring))",
    boxShadow: "0 0 0 1px hsl(var(--ring))",
  },
  ".cm-scroller": {
    fontFamily:
      "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', 'Courier New', monospace",
  },
  ".cm-content": {
    padding: "8px 12px",
    caretColor: "hsl(var(--foreground))",
  },
  ".cm-gutters": {
    backgroundColor: "transparent",
    borderRight: "1px solid hsl(var(--border))",
    color: "hsl(var(--muted-foreground))",
  },
  ".cm-activeLine": {
    backgroundColor: "hsl(var(--muted) / 0.35)",
  },
  ".cm-activeLineGutter": {
    backgroundColor: "hsl(var(--muted) / 0.35)",
  },
  // Tooltip + completion popup chrome — match shadcn surface.
  ".cm-tooltip": {
    border: "1px solid hsl(var(--border))",
    backgroundColor: "hsl(var(--popover))",
    color: "hsl(var(--popover-foreground))",
    borderRadius: "calc(var(--radius) - 2px)",
  },
  ".cm-tooltip-autocomplete > ul > li[aria-selected]": {
    backgroundColor: "hsl(var(--accent))",
    color: "hsl(var(--accent-foreground))",
  },
  ".cm-completionInfo": {
    border: "1px solid hsl(var(--border))",
    backgroundColor: "hsl(var(--popover))",
    color: "hsl(var(--popover-foreground))",
    borderRadius: "calc(var(--radius) - 2px)",
    padding: "6px 8px",
    fontSize: "0.75rem",
    maxWidth: "320px",
  },
});

// Server-side CEL linter. Debounced via `linter({ delay })`.
function makeCelLinter() {
  return linter(
    async (view) => {
      const text = view.state.doc.toString();
      if (!text.trim()) return [];
      try {
        const res = await validateRuleCel(text);
        if (res.ok) return [];
        // The validator reports a single global error; mark the whole
        // document so the gutter dot is visible regardless of cursor.
        return [
          {
            from: 0,
            to: view.state.doc.length,
            severity: "error",
            message: res.error ?? "Invalid CEL expression",
            source: "cel",
          } satisfies Diagnostic,
        ];
      } catch {
        // Transport failure — don't pollute the editor with red
        // squigglies the user can't act on.
        return [];
      }
    },
    {
      delay: 400,
    },
  );
}

export function CelEditor({
  value,
  onChange,
  minHeight = "9rem",
  className,
  readOnly,
  id,
  lint = true,
}: CelEditorProps) {
  // Fetch the live schema once per session. Falls back silently if the
  // engine isn't reachable; the completion source already has a static
  // baseline that covers the common case.
  const schemaQuery = useQuery({
    queryKey: ["rules", "schema"],
    queryFn: getRulesSchema,
    staleTime: 5 * 60_000,
    retry: false,
  });

  const catalogs = useMemo<CelExternalCatalogs>(
    () => ({
      labels: schemaQuery.data?.labels,
      liveAttributeKeys: schemaQuery.data?.attribute_keys,
    }),
    [schemaQuery.data],
  );

  const extensions = useMemo(() => {
    const exts = [
      javascript({ jsx: false, typescript: false }),
      surfaceTheme,
      autocompletion({
        override: [makeCelCompletionSource(catalogs)],
        defaultKeymap: true,
        activateOnTyping: true,
        closeOnBlur: true,
        icons: true,
      }),
    ];
    if (lint && !readOnly) exts.push(makeCelLinter());
    return exts;
  }, [catalogs, lint, readOnly]);

  return (
    <div className={cn("w-full", className)} id={id}>
      <CodeMirror
        value={value}
        onChange={onChange}
        readOnly={readOnly}
        minHeight={minHeight}
        theme={oneDark}
        extensions={extensions}
        basicSetup={{
          lineNumbers: true,
          highlightActiveLine: true,
          highlightActiveLineGutter: true,
          foldGutter: false,
          // The custom `autocompletion(...)` extension above replaces
          // basicSetup's default; keep this flag off to avoid two
          // sources stacking.
          autocompletion: false,
          searchKeymap: false,
        }}
      />
    </div>
  );
}
