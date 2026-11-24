/**
 * Edge tests: copy and attribute changes the collector applies to a page for a share of visitors, with no deploy.
 * Shared by the dashboard (validates what gets saved) and the collector (assigns and applies).
 *
 * Deliberately narrow: a variant can replace an element's text or one of a few plain attributes. It cannot add
 * markup, scripts, styles or event handlers, so whoever can write a test cannot take over the page.
 */
export interface VariantOp {
  /** CSS selector (what HTMLRewriter supports), e.g. `h1`, `a.cta`, `meta[name="description"]` */
  selector: string;
  action: "text" | "attr";
  /** for `attr`: which attribute */
  attr?: string;
  value: string;
}

export interface VariantDef {
  id: string;
  ops: VariantOp[];
}

export interface EdgeTest {
  /** also the experiment name in $exposure, so results show up under Experiments */
  id: string;
  /** exact path, or a prefix ending in "*" */
  path: string;
  status: "running" | "promoted" | "stopped";
  /** share of visitors who take part (0–1); the rest see the page untouched and are not counted */
  share: number;
  /** first variant is the control and has no ops */
  variants: VariantDef[];
  /** promoted: the variant everyone now gets */
  winner?: string;
  /** decide on its own each hour: promote a clear winner, stop a clear loser */
  auto?: boolean;
  /** unix seconds */
  startedAt: number;
  decidedAt?: number;
  /** why it was promoted or stopped */
  note?: string;
}

export const MAX_TESTS = 20;
const MAX_OPS = 8;
const MAX_VARIANTS = 4;
export const SAFE_ATTRS = ["content", "href", "alt", "title", "placeholder", "value", "aria-label"] as const;
/** Elements a variant never touches, whatever the selector matched. */
export const BLOCKED_TAGS = new Set(["script", "style", "iframe", "object", "embed", "link", "base", "noscript", "template"]);

const ID = /^[\w-]{1,40}$/;
const text = (v: unknown, max: number) => (typeof v === "string" ? v.trim().slice(0, max) : "");

/** Links may only go somewhere on this site or to https: never javascript:, data: or protocol-relative. */
const safeHref = (v: string) => /^(https:\/\/|\/(?!\/)|#|\?)/.test(v) && !/[\s"'<>]/.test(v);

export function cleanOp(input: unknown): VariantOp | null {
  if (!input || typeof input !== "object") return null;
  const o = input as Record<string, unknown>;
  const selector = text(o.selector, 200);
  if (!selector || /[<>{}\\]/.test(selector) || /\b(script|style|iframe)\b/i.test(selector)) return null;
  const value = typeof o.value === "string" ? o.value.slice(0, 600) : "";
  if (o.action === "attr") {
    const attr = text(o.attr, 20).toLowerCase();
    if (!(SAFE_ATTRS as readonly string[]).includes(attr)) return null;
    if (attr === "href" && !safeHref(value)) return null;
    return { selector, action: "attr", attr, value };
  }
  if (o.action !== "text" || !value.trim()) return null;
  return { selector, action: "text", value };
}

export function cleanEdgeTest(input: unknown): EdgeTest | null {
  if (!input || typeof input !== "object") return null;
  const t = input as Record<string, unknown>;
  const id = text(t.id, 40);
  const path = text(t.path, 200);
  if (!ID.test(id) || !/^\/[^\s'"\\]*$/.test(path)) return null;
  const seen = new Set<string>();
  const variants = (Array.isArray(t.variants) ? t.variants : [])
    .map((v, i): VariantDef | null => {
      if (!v || typeof v !== "object") return null;
      const vv = v as Record<string, unknown>;
      const vid = text(vv.id, 32);
      if (!ID.test(vid) || seen.has(vid)) return null;
      seen.add(vid);
      // The control is the page as it is.
      const ops = i === 0 ? [] : (Array.isArray(vv.ops) ? vv.ops : []).map(cleanOp).filter((o): o is VariantOp => o !== null).slice(0, MAX_OPS);
      return i > 0 && !ops.length ? null : { id: vid, ops };
    })
    .filter((v): v is VariantDef => v !== null)
    .slice(0, MAX_VARIANTS);
  if (variants.length < 2) return null;
  const status = t.status === "promoted" || t.status === "stopped" ? t.status : "running";
  const winner = text(t.winner, 32);
  if (status === "promoted" && !variants.some((v) => v.id === winner)) return null;
  const share = Number(t.share);
  const startedAt = Number(t.startedAt);
  const decidedAt = Number(t.decidedAt);
  const note = text(t.note, 200);
  return {
    id, path, status, variants,
    share: Number.isFinite(share) ? Math.min(1, Math.max(0.01, share)) : 1,
    startedAt: Number.isFinite(startedAt) && startedAt > 0 ? Math.round(startedAt) : Math.floor(Date.now() / 1000),
    ...(status === "promoted" ? { winner } : {}),
    ...(t.auto === true ? { auto: true } : {}),
    ...(Number.isFinite(decidedAt) && decidedAt > 0 ? { decidedAt: Math.round(decidedAt) } : {}),
    ...(note ? { note } : {}),
  };
}

export function cleanEdgeTests(input: unknown): EdgeTest[] {
  if (!Array.isArray(input)) return [];
  const seen = new Set<string>();
  return input
    .map(cleanEdgeTest)
    .filter((t): t is EdgeTest => t !== null && !seen.has(t.id) && (seen.add(t.id), true))
    .slice(0, MAX_TESTS);
}

export const pathMatchesTest = (pattern: string, path: string) => (pattern.endsWith("*") ? path.startsWith(pattern.slice(0, -1)) : path === pattern || path === `${pattern}/` || `${path}/` === pattern);

/** FNV-1a → [0, 1). Sync and stable: the same visitor gets the same answer on every request that day. */
export function unit(key: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < key.length; i++) {
    h ^= key.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  // Final avalanche so near-identical keys don't land near each other.
  h ^= h >>> 16;
  h = Math.imul(h, 0x85ebca6b);
  h ^= h >>> 13;
  return (h >>> 0) / 4294967296;
}

export interface Assignment {
  test: string;
  variant: VariantDef;
  /** true when the visitor takes part in a running test and must be counted */
  exposed: boolean;
}

/**
 * What one visitor gets on one path. Running: a share takes part, split evenly; everyone else sees the page as is.
 * Promoted: everyone gets the winner and nobody is counted. One test per page: the first that matches.
 * `takesPart: false` (crawlers, bots) only ever gets a promoted winner.
 */
export function assign(tests: readonly EdgeTest[], path: string, visitor: string, takesPart = true): Assignment | null {
  for (const t of tests) {
    if (t.status === "stopped" || !pathMatchesTest(t.path, path)) continue;
    if (t.status === "promoted") {
      const w = t.variants.find((v) => v.id === t.winner);
      return w && w.ops.length ? { test: t.id, variant: w, exposed: false } : null;
    }
    if (!takesPart || unit(`${visitor}|${t.id}|in`) >= t.share) return null;
    const i = Math.min(t.variants.length - 1, Math.floor(unit(`${visitor}|${t.id}|arm`) * t.variants.length));
    return { test: t.id, variant: t.variants[i]!, exposed: true };
  }
  return null;
}
