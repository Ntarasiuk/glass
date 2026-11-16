import { toDataPoint, type Row } from "./schema.ts";

export interface AgentLensEnv {
  EVENTS: AnalyticsEngineDataset;
  AL_SECRET?: string;
  INJECT?: string; // "false" disables beacon injection
  LOG_ASSETS?: string; // "true" logs static assets requested by humans
  DEBUG_LOG?: string; // "true" logs every data point (local dev)
}

export interface ObserveOptions {
  /** Rewrite paths before they are stored, e.g. to drop usernames or secret slugs. */
  redactPath?: (path: string) => string;
}

export const projectOf = (host: string) => host.replace(/^www\./, "");

export const logErr = (msg: string) => (err: unknown) => console.error(JSON.stringify({ msg, err: String(err) }));

export function write(env: AgentLensEnv, row: Row) {
  if (env.DEBUG_LOG === "true") console.log(JSON.stringify({ msg: "datapoint", ...row }));
  env.EVENTS.writeDataPoint(toDataPoint(row));
}

type Cf = Record<string, unknown>;
const clean = (x: unknown) => String(x ?? "").replace(/\|/g, " ").slice(0, 60);
const coord = (x: unknown) => {
  const n = Number(x);
  return Number.isFinite(n) ? Math.round(n * 10) / 10 : 0;
};

/** City-level location from request.cf: never the IP itself. */
export function geoOf(cf: Cf): { geo: string; lat: number; lon: number } {
  const geo = [clean(cf.country), clean(cf.region), clean(cf.city)].join("|").replace(/\|+$/, "");
  return { geo, lat: coord(cf.latitude), lon: coord(cf.longitude) };
}
