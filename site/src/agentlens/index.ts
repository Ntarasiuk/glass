/**
 * AgentLens: agent-aware analytics, vendored from github.com/Ntarasiuk/agentlens (src/core + src/collector).
 * Classifies every request it sees (human, agentic browser, AI fetcher/crawler, script, bot), verifies claimed
 * bot identity (Web Bot Auth signatures, operator IP ranges), inlines a cookieless beacon into HTML pages, and
 * writes rows to the `agentlens_events` Analytics Engine dataset. Recording runs in waitUntil and never changes
 * a response except for the injected <script>. Off unless both the AGENTLENS binding and AL_SECRET exist.
 * Update by re-copying from the upstream repo; keep edits upstream.
 */
import { observe } from './observe.ts';
import { trackServerEvent } from './server-event.ts';

export interface AgentLensBindings {
  AGENTLENS?: AnalyticsEngineDataset;
  AL_SECRET?: string;
}

const lens = (env: AgentLensBindings) => (env.AGENTLENS && env.AL_SECRET ? { EVENTS: env.AGENTLENS, AL_SECRET: env.AL_SECRET } : null);

export function withAgentLens(req: Request, env: AgentLensBindings, ctx: ExecutionContext, next: (req: Request) => Promise<Response>): Promise<Response> {
  const l = lens(env);
  return l ? observe(req, l, ctx, next) : next(req);
}

/** A goal that happens on the server (install script, release download), classified like any request. */
export function trackGoal(req: Request, env: AgentLensBindings, ctx: ExecutionContext, name: string, props?: Record<string, string | number | boolean>): void {
  const l = lens(env);
  if (l) trackServerEvent(req, l, ctx, name, props);
}
