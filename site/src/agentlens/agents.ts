import type { TrafficClass } from "./taxonomy.ts";

export interface AgentDef {
  name: string;
  operator: string;
  cls: TrafficClass;
  ua: RegExp;
  /** key into RANGE_LISTS for IP verification */
  ranges?: RangeKey;
}

/** Operator-published IP lists (JSON: { prefixes: [{ ipv4Prefix | ipv6Prefix }] }). */
export const RANGE_LISTS = {
  "openai-gptbot": "https://openai.com/gptbot.json",
  "openai-search": "https://openai.com/searchbot.json",
  "openai-user": "https://openai.com/chatgpt-user.json",
  anthropic: "https://claude.com/crawling/bots.json",
  "perplexity-bot": "https://www.perplexity.ai/perplexitybot.json",
  "perplexity-user": "https://www.perplexity.ai/perplexity-user.json",
  google: "https://developers.google.com/static/crawling/ipranges/common-crawlers.json",
  bing: "https://www.bing.com/toolbox/bingbot.json",
} as const;
export type RangeKey = keyof typeof RANGE_LISTS;

/** First match wins: keep specific patterns above generic ones. */
export const AGENTS: AgentDef[] = [
  // OpenAI
  { name: "ChatGPT-User", operator: "OpenAI", cls: "ai_user_fetcher", ua: /ChatGPT-User/i, ranges: "openai-user" },
  { name: "OAI-SearchBot", operator: "OpenAI", cls: "ai_search_crawler", ua: /OAI-SearchBot/i, ranges: "openai-search" },
  { name: "GPTBot", operator: "OpenAI", cls: "ai_training_crawler", ua: /GPTBot/i, ranges: "openai-gptbot" },
  // Anthropic
  { name: "Claude-User", operator: "Anthropic", cls: "ai_user_fetcher", ua: /Claude-User/i, ranges: "anthropic" },
  { name: "Claude-SearchBot", operator: "Anthropic", cls: "ai_search_crawler", ua: /Claude-SearchBot/i, ranges: "anthropic" },
  { name: "ClaudeBot", operator: "Anthropic", cls: "ai_training_crawler", ua: /ClaudeBot|anthropic-ai/i, ranges: "anthropic" },
  // Perplexity
  { name: "Perplexity-User", operator: "Perplexity", cls: "ai_user_fetcher", ua: /Perplexity-User/i, ranges: "perplexity-user" },
  { name: "PerplexityBot", operator: "Perplexity", cls: "ai_search_crawler", ua: /PerplexityBot/i, ranges: "perplexity-bot" },
  // Other AI
  { name: "MistralAI-User", operator: "Mistral", cls: "ai_user_fetcher", ua: /MistralAI-User/i },
  { name: "Meta-ExternalFetcher", operator: "Meta", cls: "ai_user_fetcher", ua: /Meta-ExternalFetcher/i },
  { name: "Meta-ExternalAgent", operator: "Meta", cls: "ai_training_crawler", ua: /Meta-ExternalAgent/i },
  { name: "Google-NotebookLM", operator: "Google", cls: "ai_user_fetcher", ua: /Google-NotebookLM/i },
  { name: "cohere-ai", operator: "Cohere", cls: "ai_user_fetcher", ua: /cohere-ai/i },
  { name: "cohere-training-data-crawler", operator: "Cohere", cls: "ai_training_crawler", ua: /cohere-training-data-crawler/i },
  { name: "DuckAssistBot", operator: "DuckDuckGo", cls: "ai_search_crawler", ua: /DuckAssistBot/i },
  { name: "YouBot", operator: "You.com", cls: "ai_search_crawler", ua: /YouBot/i },
  { name: "Bytespider", operator: "ByteDance", cls: "ai_training_crawler", ua: /Bytespider/i },
  { name: "CCBot", operator: "Common Crawl", cls: "ai_training_crawler", ua: /CCBot/i },
  { name: "Amazonbot", operator: "Amazon", cls: "ai_training_crawler", ua: /Amazonbot/i },
  { name: "Applebot", operator: "Apple", cls: "search_crawler", ua: /Applebot/i },
  { name: "AI2Bot", operator: "Allen AI", cls: "ai_training_crawler", ua: /AI2Bot/i },
  { name: "Diffbot", operator: "Diffbot", cls: "ai_training_crawler", ua: /Diffbot/i },
  { name: "Timpibot", operator: "Timpi", cls: "ai_training_crawler", ua: /Timpibot/i },
  { name: "ImagesiftBot", operator: "ImageSift", cls: "ai_training_crawler", ua: /ImagesiftBot/i },
  { name: "PanguBot", operator: "Huawei", cls: "ai_training_crawler", ua: /PanguBot/i },
  { name: "omgili", operator: "Webz.io", cls: "ai_training_crawler", ua: /omgili/i },
  // Classic search
  { name: "Googlebot", operator: "Google", cls: "search_crawler", ua: /Googlebot|Google-InspectionTool|GoogleOther/i, ranges: "google" },
  { name: "Bingbot", operator: "Microsoft", cls: "search_crawler", ua: /bingbot/i, ranges: "bing" },
  { name: "DuckDuckBot", operator: "DuckDuckGo", cls: "search_crawler", ua: /DuckDuckBot/i },
  { name: "YandexBot", operator: "Yandex", cls: "search_crawler", ua: /YandexBot/i },
  { name: "Baiduspider", operator: "Baidu", cls: "search_crawler", ua: /Baiduspider/i },
  { name: "PetalBot", operator: "Huawei", cls: "search_crawler", ua: /PetalBot/i },
  // SEO
  { name: "AhrefsBot", operator: "Ahrefs", cls: "seo_crawler", ua: /AhrefsBot/i },
  { name: "SemrushBot", operator: "Semrush", cls: "seo_crawler", ua: /SemrushBot/i },
  { name: "MJ12bot", operator: "Majestic", cls: "seo_crawler", ua: /MJ12bot/i },
  { name: "DotBot", operator: "Moz", cls: "seo_crawler", ua: /DotBot|rogerbot/i },
  { name: "Screaming Frog", operator: "Screaming Frog", cls: "seo_crawler", ua: /Screaming Frog/i },
  // Social link previews
  { name: "facebookexternalhit", operator: "Meta", cls: "social_preview", ua: /facebookexternalhit|facebookcatalog/i },
  { name: "Twitterbot", operator: "X", cls: "social_preview", ua: /Twitterbot/i },
  { name: "Slackbot", operator: "Slack", cls: "social_preview", ua: /Slackbot|Slack-ImgProxy/i },
  { name: "Discordbot", operator: "Discord", cls: "social_preview", ua: /Discordbot/i },
  { name: "LinkedInBot", operator: "LinkedIn", cls: "social_preview", ua: /LinkedInBot/i },
  { name: "TelegramBot", operator: "Telegram", cls: "social_preview", ua: /TelegramBot/i },
  { name: "WhatsApp", operator: "Meta", cls: "social_preview", ua: /WhatsApp/i },
  { name: "redditbot", operator: "Reddit", cls: "social_preview", ua: /redditbot/i },
  { name: "Embedly", operator: "Embedly", cls: "social_preview", ua: /Embedly|Iframely/i },
  // Monitoring
  { name: "UptimeRobot", operator: "UptimeRobot", cls: "monitor", ua: /UptimeRobot/i },
  { name: "Pingdom", operator: "Pingdom", cls: "monitor", ua: /Pingdom/i },
  { name: "Better Stack", operator: "Better Stack", cls: "monitor", ua: /Better ?Uptime|BetterStack/i },
  { name: "StatusCake", operator: "StatusCake", cls: "monitor", ua: /StatusCake/i },
  { name: "Datadog Synthetics", operator: "Datadog", cls: "monitor", ua: /DatadogSynthetics/i },
  { name: "Checkly", operator: "Checkly", cls: "monitor", ua: /Checkly/i },
  // Automation
  { name: "Headless Chrome", operator: "", cls: "automation", ua: /HeadlessChrome/i },
  { name: "PhantomJS", operator: "", cls: "automation", ua: /PhantomJS/i },
  { name: "curl", operator: "", cls: "automation", ua: /^curl\//i },
  { name: "Wget", operator: "", cls: "automation", ua: /^Wget/i },
  { name: "python", operator: "", cls: "automation", ua: /python-requests|python-httpx|aiohttp|python-urllib|Python\//i },
  { name: "Go http", operator: "", cls: "automation", ua: /Go-http-client/i },
  { name: "Node.js", operator: "", cls: "automation", ua: /node-fetch|undici|axios|got \(|Node\.js/i },
  { name: "Java", operator: "", cls: "automation", ua: /^Java\/|okhttp|Apache-HttpClient/i },
  { name: "Scrapy", operator: "", cls: "automation", ua: /Scrapy/i },
  { name: "libwww-perl", operator: "", cls: "automation", ua: /libwww-perl/i },
];

/** Known Web Bot Auth signers, keyed by Signature-Agent host. */
export const SIGNED_AGENTS: Record<string, { name: string; operator: string; cls: TrafficClass }> = {
  "chatgpt.com": { name: "ChatGPT agent", operator: "OpenAI", cls: "agentic_browser" },
};

/** Cloud/hosting ASNs: a "browser" from here is most likely a cloud-run agent or scraper. */
export const DATACENTER_ASNS = new Set<number>([
  16509, 14618, // AWS
  396982, // Google Cloud
  8075, // Microsoft Azure
  14061, // DigitalOcean
  24940, // Hetzner
  16276, // OVH
  63949, // Linode/Akamai Cloud
  31898, // Oracle Cloud
  45102, // Alibaba
  132203, // Tencent
  20473, // Vultr
  212238, // Datacamp/CDN77
  9009, // M247
]);

const BROWSER_UA = /^Mozilla\/5\.0 .*(Chrome|Firefox|Safari|Edg|OPR)\//;
const GENERIC_BOT = /bot|crawl|spider|scrape|fetch|slurp|archiver|preview|http-?client|headless/i;

export const isBrowserUA = (ua: string) => BROWSER_UA.test(ua);
export const looksLikeBot = (ua: string) => GENERIC_BOT.test(ua);
export const matchAgent = (ua: string): AgentDef | undefined => AGENTS.find((a) => a.ua.test(ua));
