// Inline SVG icon library + `iconButton()` helper. Stroke-based
// feather-style for visual consistency with the v1 nexus-admin
// console (matching SVG path data, sizing, and `.icon-btn` CSS
// rules). All icons render at 14×14 by default via the
// `.icon-btn svg` rule in `ui/src/ui/styles.css`.
//
// Pattern is identical to `nexus-admin/static/js/utils.js`'s
// `ICONS` map + `iconButton()` so design choices stay in sync
// across both admin surfaces. The TS port adds typed `IconKind`
// + the `h()`-based DOM construction the rest of this SPA uses.

import { h } from "./el.js";

export type IconKind = "gear" | "trash" | "plus" | "search" | "close" | "check";

const SVG_ATTR =
  'viewBox="0 0 24 24" width="14" height="14" fill="none" ' +
  'stroke="currentColor" stroke-width="2" stroke-linecap="round" ' +
  'stroke-linejoin="round" aria-hidden="true"';

const ICON_PATHS: Record<IconKind, string> = {
  gear:
    '<circle cx="12" cy="12" r="3"/>' +
    '<path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 ' +
    "2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 " +
    "1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33" +
    "l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 " +
    ".33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9" +
    "a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06" +
    ".06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09" +
    'a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 ' +
    "0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 " +
    '1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/>',
  trash:
    '<polyline points="3 6 5 6 21 6"/>' +
    '<path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6"/>' +
    '<path d="M10 11v6"/><path d="M14 11v6"/>' +
    '<path d="M9 6V4a2 2 0 0 1 2-2h2a2 2 0 0 1 2 2v2"/>',
  plus:
    '<line x1="12" y1="5" x2="12" y2="19"/>' +
    '<line x1="5" y1="12" x2="19" y2="12"/>',
  search:
    '<circle cx="11" cy="11" r="7"/>' +
    '<line x1="21" y1="21" x2="16.65" y2="16.65"/>',
  close:
    '<line x1="18" y1="6" x2="6" y2="18"/>' +
    '<line x1="6" y1="6" x2="18" y2="18"/>',
  check: '<polyline points="20 6 9 17 4 12"/>',
};

/// Returns a detached `<svg>` element for the given icon kind.
/// Use this when you need to inline an icon inside a non-button
/// element (e.g. inside `+ New camera` next to text); for the
/// common edit/delete icon-only-button pattern, use `iconButton()`.
export function icon(kind: IconKind): SVGElement {
  const wrapper = h("span", null);
  wrapper.innerHTML = `<svg ${SVG_ATTR}>${ICON_PATHS[kind]}</svg>`;
  // innerHTML guarantees one <svg> child — cast is safe.
  return wrapper.firstElementChild as SVGElement;
}

export interface IconButtonOpts {
  /// `title` + `aria-label` for the button. Required because
  /// icon-only buttons have no visible text.
  title: string;
  onClick?: () => void;
  /// Adds the `.danger` class so hover paints the icon red and
  /// any keyboard focus ring reads as "destructive". Use for
  /// delete actions.
  danger?: boolean;
  disabled?: boolean;
  /// Extra class names to append (space-separated).
  extraClass?: string;
}

/// Build a 28×26 icon-only button. Mirrors `nexus-admin`'s
/// `iconButton(kind, title, onClick, {danger})` API so muscle
/// memory transfers between the two admin codebases. The result
/// is a fully-constructed `<button type="button">` ready to drop
/// into a row action cell.
export function iconButton(kind: IconKind, opts: IconButtonOpts): HTMLButtonElement {
  const classes = ["icon-btn", `icon-btn--${kind}`];
  if (opts.danger) classes.push("danger");
  if (opts.extraClass) classes.push(opts.extraClass);
  const btn = h(
    "button",
    {
      type: "button",
      class: classes.join(" "),
      title: opts.title,
      ...(opts.disabled ? { disabled: true } : {}),
      ...(opts.onClick ? { on: { click: opts.onClick } } : {}),
    },
    icon(kind),
  );
  // `aria-label` isn't in the typed prop bag for `h()`, so set it
  // imperatively. Icon-only buttons NEED a visible-to-AT label —
  // `title` alone is not exposed by every screen reader.
  btn.setAttribute("aria-label", opts.title);
  return btn;
}
