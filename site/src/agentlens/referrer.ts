/** Hosts (suffix match) of AI assistants that send humans to your site. */
const AI_HOSTS: [string, string][] = [
  ["chatgpt.com", "chatgpt"],
  ["chat.openai.com", "chatgpt"],
  ["perplexity.ai", "perplexity"],
  ["claude.ai", "claude"],
  ["gemini.google.com", "gemini"],
  ["bard.google.com", "gemini"],
  ["notebooklm.google.com", "notebooklm"],
  ["copilot.microsoft.com", "copilot"],
  ["copilot.cloud.microsoft", "copilot"],
  ["meta.ai", "meta_ai"],
  ["grok.com", "grok"],
  ["chat.deepseek.com", "deepseek"],
  ["chat.mistral.ai", "mistral"],
  ["poe.com", "poe"],
  ["you.com", "you"],
  ["phind.com", "phind"],
  ["chat.qwen.ai", "qwen"],
  ["kimi.com", "kimi"],
  ["t3.chat", "t3chat"],
];

/** utm_source values AI apps append (e.g. ChatGPT adds utm_source=chatgpt.com). */
const AI_UTM: [RegExp, string][] = [
  [/chatgpt|openai/i, "chatgpt"],
  [/perplexity/i, "perplexity"],
  [/claude|anthropic/i, "claude"],
  [/gemini|bard/i, "gemini"],
  [/copilot/i, "copilot"],
  [/grok/i, "grok"],
  [/deepseek/i, "deepseek"],
  [/mistral|lechat/i, "mistral"],
];

export function hostOf(url: string | null | undefined): string {
  if (!url) return "";
  try {
    return new URL(url).hostname.toLowerCase();
  } catch {
    return "";
  }
}

export function aiSourceFromHost(host: string): string {
  for (const [h, src] of AI_HOSTS) if (host === h || host.endsWith("." + h)) return src;
  return "";
}

/** referrer host wins; falls back to utm_source because many AI apps strip Referer. */
export function aiSource(referrerHost: string, utmSource: string): string {
  const fromRef = aiSourceFromHost(referrerHost);
  if (fromRef) return fromRef;
  if (!utmSource) return "";
  const fromUtmHost = aiSourceFromHost(utmSource.toLowerCase());
  if (fromUtmHost) return fromUtmHost;
  for (const [re, src] of AI_UTM) if (re.test(utmSource)) return src;
  return "";
}
