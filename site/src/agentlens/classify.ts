import { DATACENTER_ASNS, isBrowserUA, looksLikeBot, matchAgent } from "./agents.ts";
import type { TrafficClass, Verdict } from "./taxonomy.ts";

export interface RequestFacts {
  ua: string;
  headers: Headers;
  asn?: number;
  /** request.cf.botManagement (Enterprise Bot Management only) */
  cfBot?: { score?: number; verifiedBot?: boolean };
}

/**
 * Synchronous, header-only classification. Runs on the hot path, so no I/O.
 * Identity verification (IP ranges, Web Bot Auth) happens later, off the response path.
 */
export function classifyRequest(f: RequestFacts): Verdict {
  const { ua, headers } = f;
  const signals: string[] = [];

  if (headers.has("signature-agent") || headers.has("signature-input")) signals.push("signed_request");

  const known = matchAgent(ua);
  if (known) {
    return {
      cls: known.cls,
      agent: known.name,
      operator: known.operator,
      verification: "unverified",
      confidence: known.cls === "automation" ? 0.9 : 0.8,
      signals: [...signals, "ua_match"],
      rangeKey: known.ranges,
    };
  }

  if (!ua) return v("unknown_bot", "", 0.9, [...signals, "empty_ua"]);

  if (!isBrowserUA(ua)) {
    return v(looksLikeBot(ua) ? "unknown_bot" : "automation", "", 0.7, [...signals, "non_browser_ua"]);
  }

  // Claims to be a browser. Look for evidence it isn't a person.
  const noLang = !headers.has("accept-language");
  const noFetchMeta = !headers.has("sec-fetch-mode");
  if (noLang && noFetchMeta) signals.push("missing_browser_headers");
  const dc = f.asn !== undefined && DATACENTER_ASNS.has(f.asn);
  if (dc) signals.push("datacenter_asn");
  if (f.cfBot?.score !== undefined && f.cfBot.score < 30) signals.push(`cf_bot_score:${f.cfBot.score}`);

  if (signals.includes("missing_browser_headers")) {
    return v("automation", "Browser impersonator", 0.75, signals);
  }
  if (signals.includes("signed_request")) {
    // Real browser that signs its requests: an agent. Identity resolved in verifySignature.
    return v("agentic_browser", "Signed agent", 0.7, signals);
  }
  if (dc) return v("agentic_browser", "Cloud browser", 0.55, signals);
  if (f.cfBot?.score !== undefined && f.cfBot.score < 30) return v("automation", "", 0.6, signals);

  return v("human", "", 0.6, signals);
}

const v = (cls: TrafficClass, agent: string, confidence: number, signals: string[]): Verdict => ({
  cls,
  agent,
  operator: "",
  verification: "none",
  confidence,
  signals,
});

/** Client-side evidence sent by the beacon. */
export interface BeaconSignals {
  static: string[]; // webdriver, automation globals, headless hints
  dwellMs?: number;
  mouseMoves?: number;
  scrolls?: number;
  keys?: number;
  clicks?: number;
  clicksWithoutMove?: number;
  touches?: number;
  untrusted?: number;
  straightness?: number; // displacement / path length of the mouse, 1 = perfectly straight
}

const STRONG = /^(webdriver|g:|headless_ua)/;

/** Refine the edge verdict with what the page saw in JS. Returns class + extra signals. */
export function refineWithBeacon(
  edge: TrafficClass,
  s: BeaconSignals,
): { cls: TrafficClass; confidence: number; signals: string[] } {
  const signals = s.static.slice(0, 12);
  let score = 0;
  if (signals.some((x) => STRONG.test(x))) score = Math.max(score, 0.95);
  if ((s.untrusted ?? 0) > 0) {
    signals.push("synthetic_events");
    score = Math.max(score, 0.85);
  }
  const clicks = s.clicks ?? 0;
  if (clicks > 0 && (s.clicksWithoutMove ?? 0) === clicks && !s.mouseMoves && !s.touches) {
    signals.push("click_without_movement");
    score = Math.max(score, 0.7);
  }
  if ((s.mouseMoves ?? 0) >= 5 && (s.straightness ?? 0) > 0.985) {
    signals.push("linear_mouse");
    score = Math.max(score, 0.6);
  }

  if (edge === "human" || edge === "agentic_browser") {
    if (score >= 0.6) return { cls: "agentic_browser", confidence: score, signals };
    return { cls: edge, confidence: edge === "human" ? 0.7 : 0.55, signals };
  }
  // A crawler/fetcher class that executed JS keeps its class; the beacon row itself is the signal.
  return { cls: edge, confidence: 0.8, signals: [...signals, "executed_js"] };
}
