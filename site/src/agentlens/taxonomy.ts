import type { RangeKey } from "./agents.ts";

/** Every request lands in exactly one class. Order = rough "how human" ranking. */
export const CLASSES = [
  "human",
  "agentic_browser", // real browser driven by an AI agent (Operator/ChatGPT agent, Comet, cloud browsers)
  "ai_user_fetcher", // live fetch on behalf of a user prompt (ChatGPT-User, Claude-User, Perplexity-User)
  "ai_search_crawler", // indexing for AI answers (OAI-SearchBot, Claude-SearchBot, PerplexityBot)
  "ai_training_crawler", // bulk collection for model training (GPTBot, ClaudeBot, CCBot)
  "search_crawler", // classic search engines (Googlebot, Bingbot)
  "seo_crawler",
  "social_preview",
  "monitor",
  "automation", // scripts, headless browsers, HTTP libraries, browser-impersonators
  "unknown_bot",
] as const;

export type TrafficClass = (typeof CLASSES)[number];

export const AI_CLASSES: readonly TrafficClass[] = [
  "agentic_browser",
  "ai_user_fetcher",
  "ai_search_crawler",
  "ai_training_crawler",
];

/**
 * How sure we are about *who* it is.
 * signed      – valid Web Bot Auth (HTTP Message Signatures) signature. Cryptographic.
 * ip_verified – claimed bot UA and source IP is inside the operator's published ranges.
 * unverified  – claimed identity, no way to check (no published ranges / list unavailable).
 * spoofed     – claimed a bot identity that failed verification (wrong IPs or bad signature).
 * none        – no identity claimed (browsers, generic clients).
 */
export type Verification = "signed" | "ip_verified" | "unverified" | "spoofed" | "none";

export interface Verdict {
  cls: TrafficClass;
  agent: string; // display name, "" for humans
  operator: string;
  verification: Verification;
  confidence: number; // 0..1, confidence in `cls`
  signals: string[]; // why we decided
  rangeKey?: RangeKey; // published IP list to verify against (internal)
}

export const classIndex = (c: TrafficClass): number => CLASSES.indexOf(c);
export const classAt = (i: number): TrafficClass | undefined => CLASSES[i];
