export interface Client {
  browser: string;
  os: string;
  device: "desktop" | "mobile" | "tablet" | "bot";
}

const BROWSERS: [RegExp, string][] = [
  [/FBAN|FBAV|FB_IAB/, "Facebook app"],
  [/Instagram/, "Instagram app"],
  [/Edg(A|iOS)?\//, "Edge"],
  [/OPR\/|Opera/, "Opera"],
  [/SamsungBrowser/, "Samsung Internet"],
  [/Firefox\/|FxiOS/, "Firefox"],
  [/Chrome\/|CriOS/, "Chrome"],
  [/Version\/[\d.]+.*Safari/, "Safari"],
];

const OSES: [RegExp, string][] = [
  [/iPhone|iPad|iPod/, "iOS"],
  [/Android/, "Android"],
  [/Windows/, "Windows"],
  [/CrOS/, "ChromeOS"],
  [/Mac OS X|Macintosh/, "macOS"],
  [/Linux/, "Linux"],
];

/**
 * Coarse browser / OS / device from the UA, refined by what the page reports:
 * iPadOS sends a Mac UA, so a "Mac" with touch points is a tablet.
 */
export function parseClient(ua: string, touchPoints = 0, isBot = false): Client {
  const browser = BROWSERS.find(([re]) => re.test(ua))?.[1] ?? "Other";
  let os = OSES.find(([re]) => re.test(ua))?.[1] ?? "Other";
  let device: Client["device"] = "desktop";
  if (isBot) device = "bot";
  else if (/iPad|Tablet/.test(ua) || (/Android/.test(ua) && !/Mobile/.test(ua))) device = "tablet";
  else if (/Mobi|iPhone|iPod/.test(ua)) device = "mobile";
  else if (os === "macOS" && touchPoints > 1) {
    device = "tablet";
    os = "iOS";
  }
  return { browser, os, device };
}

/** Packed into one column as "browser|os|device"; the dashboard splits it. */
export const packClient = (c: Client) => `${c.browser}|${c.os}|${c.device}`;
