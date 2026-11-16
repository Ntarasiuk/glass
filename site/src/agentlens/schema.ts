/**
 * Analytics Engine row layout. Column order IS the schema: append, never reorder.
 * index1 = project (site host). Max 20 blobs, 20 doubles, 16 KB of blobs per row.
 */
export const BLOBS = [
  "source", // blob1  edge | beacon | server (server-side events)
  "kind", // blob2  request | pv | engage | event
  "cls", // blob3  final traffic class
  "agent", // blob4
  "operator", // blob5
  "verification", // blob6
  "client", // blob7  "browser|os|device"
  "path", // blob8
  "utm_medium", // blob9
  "referrer_host", // blob10
  "ai_source", // blob11
  "utm_source", // blob12
  "geo", // blob13 "CC|Region|City" (country code first; older rows hold just "CC")
  "asn_org", // blob14
  "ua", // blob15
  "signals", // blob16 comma-separated
  "event_name", // blob17
  "visitor", // blob18
  "edge_cls", // blob19 class assigned at the edge (beacon rows: before JS refinement)
  "detail", // blob20 content-type (edge) | utm_campaign (pv) | event props JSON (event)
] as const;

export const DOUBLES = [
  "status", // double1
  "duration_ms", // double2 origin time (edge) | dwell (engage)
  "confidence", // double3
  "asn", // double4
  "is_html", // double5
  "cf_bot_score", // double6 -1 if unavailable
  "interactions", // double7
  "value", // double8 event value
  "bytes", // double9 response content-length, -1 if unknown
  "is_entry", // double10 1 = landing pageview (referrer is not this site)
  "viewport", // double11 window width reported by the beacon
  "lat", // double12 city-level latitude from Cloudflare, 1 decimal (~11 km); 0 = unknown
  "lon", // double13 city-level longitude, 1 decimal
  "lcp", // double14 Largest Contentful Paint, ms (engage rows; 0 = not measured)
  "inp", // double15 Interaction to Next Paint, ms
  "layout_shift", // double16 Cumulative Layout Shift × 1000
  "ttfb", // double17 Time to First Byte, ms
  "scroll", // double18 deepest scroll on the page, % of its height
] as const;

export type BlobName = (typeof BLOBS)[number];
export type DoubleName = (typeof DOUBLES)[number];
export type Row = { index: string } & Partial<Record<BlobName, string>> & Partial<Record<DoubleName, number>>;

const LIMITS: Partial<Record<BlobName, number>> = { ua: 256, path: 512, detail: 1024, signals: 256 };

export function toDataPoint(r: Row): AnalyticsEngineDataPoint {
  return {
    indexes: [r.index.slice(0, 96)],
    blobs: BLOBS.map((b) => (r[b] ?? "").slice(0, LIMITS[b] ?? 128)),
    doubles: DOUBLES.map((d) => r[d] ?? 0),
  };
}

/** SQL column name for a logical field, e.g. col("cls") → "blob3". */
export function col(name: BlobName | DoubleName): string {
  const b = BLOBS.indexOf(name as BlobName);
  if (b >= 0) return `blob${b + 1}`;
  const d = DOUBLES.indexOf(name as DoubleName);
  if (d >= 0) return `double${d + 1}`;
  throw new Error(`unknown column ${name}`);
}
