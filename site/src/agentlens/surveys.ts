/**
 * One-question surveys: a small card the collector adds to a page for a share of visitors, asking why.
 * Shared by the dashboard (validates what gets saved) and the collector (decides who is asked).
 *
 * The question and the options are plain text, shown with textContent: whoever can write a survey cannot add
 * markup or script to the page.
 */
import { pathMatchesTest, unit } from "./variants.ts";

export interface Survey {
  /** also the `survey` property on the $survey events */
  id: string;
  /** exact path, or a prefix ending in "*" */
  path: string;
  question: string;
  /** 0–5 answers to click; with none, `other` is forced on */
  options: string[];
  /** also offer a short free-text answer */
  other: boolean;
  /** share of visitors asked (0–1) */
  share: number;
  /** seconds on the page before the card appears */
  after: number;
  status: "running" | "stopped";
  /** unix seconds */
  startedAt: number;
  stoppedAt?: number;
}

export const MAX_SURVEYS = 10;
export const MAX_ANSWER = 140;
const ID = /^[\w-]{1,40}$/;
const text = (v: unknown, max: number) => (typeof v === "string" ? v.replace(/\s+/g, " ").trim().slice(0, max) : "");

export function cleanSurvey(input: unknown): Survey | null {
  if (!input || typeof input !== "object") return null;
  const o = input as Record<string, unknown>;
  const id = text(o.id, 40), path = text(o.path, 200), question = text(o.question, 140);
  if (!ID.test(id) || !path.startsWith("/") || /[\s"'<>]/.test(path) || question.length < 3) return null;
  const options = [...new Set((Array.isArray(o.options) ? o.options : []).map((x) => text(x, 60)).filter(Boolean))].slice(0, 5);
  const share = Number(o.share), after = Number(o.after), startedAt = Number(o.startedAt), stoppedAt = Number(o.stoppedAt);
  return {
    id, path, question, options,
    other: o.other === true || options.length === 0,
    share: Number.isFinite(share) && share > 0 ? Math.min(1, Math.max(0.01, share)) : 1,
    after: Number.isFinite(after) ? Math.min(120, Math.max(0, Math.round(after))) : 15,
    status: o.status === "stopped" ? "stopped" : "running",
    startedAt: Number.isFinite(startedAt) && startedAt > 0 ? Math.floor(startedAt) : 0,
    ...(Number.isFinite(stoppedAt) && stoppedAt > 0 ? { stoppedAt: Math.floor(stoppedAt) } : {}),
  };
}

export function cleanSurveys(input: unknown): Survey[] {
  const seen = new Set<string>();
  const out: Survey[] = [];
  for (const x of Array.isArray(input) ? input : []) {
    const s = cleanSurvey(x);
    if (!s || seen.has(s.id)) continue;
    seen.add(s.id);
    out.push(s);
    if (out.length >= MAX_SURVEYS) break;
  }
  return out;
}

/** The survey one visitor is asked on one path, if any. One per page: the first running one that matches. */
export function pickSurvey(surveys: readonly Survey[], path: string, visitor: string): Survey | null {
  for (const s of surveys) {
    if (s.status !== "running" || !pathMatchesTest(s.path, path)) continue;
    return unit(`${visitor}|${s.id}|ask`) < s.share ? s : null;
  }
  return null;
}
