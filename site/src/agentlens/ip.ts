export interface Net {
  v: 4 | 6;
  base: bigint;
  prefix: number;
}

export function parseIp(ip: string): { v: 4 | 6; n: bigint } | null {
  if (ip.includes(":")) {
    const n = parseV6(ip);
    return n === null ? null : { v: 6, n };
  }
  const n = parseV4(ip);
  return n === null ? null : { v: 4, n };
}

function parseV4(ip: string): bigint | null {
  const parts = ip.split(".");
  if (parts.length !== 4) return null;
  let n = 0n;
  for (const p of parts) {
    if (!/^\d{1,3}$/.test(p)) return null;
    const x = Number(p);
    if (x > 255) return null;
    n = (n << 8n) | BigInt(x);
  }
  return n;
}

function parseV6(ip: string): bigint | null {
  let s = ip.split("%")[0]!; // drop zone id
  // embedded IPv4 tail (::ffff:1.2.3.4)
  const lastColon = s.lastIndexOf(":");
  const tail = s.slice(lastColon + 1);
  if (tail.includes(".")) {
    const v4 = parseV4(tail);
    if (v4 === null) return null;
    s = s.slice(0, lastColon + 1) + ((v4 >> 16n) & 0xffffn).toString(16) + ":" + (v4 & 0xffffn).toString(16);
  }
  const halves = s.split("::");
  if (halves.length > 2) return null;
  const head = halves[0] ? halves[0].split(":") : [];
  const rest = halves.length === 2 && halves[1] ? halves[1].split(":") : [];
  const missing = 8 - head.length - rest.length;
  if (halves.length === 1 ? missing !== 0 : missing < 1) return null;
  const groups = [...head, ...Array<string>(halves.length === 2 ? missing : 0).fill("0"), ...rest];
  let n = 0n;
  for (const g of groups) {
    if (!/^[0-9a-f]{1,4}$/i.test(g)) return null;
    n = (n << 16n) | BigInt(parseInt(g, 16));
  }
  return n;
}

export function parseCidr(cidr: string): Net | null {
  const [addr, len] = cidr.split("/");
  const ip = parseIp(addr ?? "");
  if (!ip) return null;
  const bits = ip.v === 4 ? 32 : 128;
  const prefix = len === undefined ? bits : Number(len);
  if (!Number.isInteger(prefix) || prefix < 0 || prefix > bits) return null;
  return { v: ip.v, base: ip.n, prefix };
}

export function inNets(ip: string, nets: readonly Net[]): boolean {
  const p = parseIp(ip);
  if (!p) return false;
  const bits = p.v === 4 ? 32 : 128;
  for (const net of nets) {
    if (net.v !== p.v) continue;
    const shift = BigInt(bits - net.prefix);
    if (p.n >> shift === net.base >> shift) return true;
  }
  return false;
}
