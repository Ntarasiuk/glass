import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
// Generates the docs in every format from docs.md (the single source), and copies the
// latest release into public/. Run before `wrangler deploy` (npm run deploy does).

import { copyFileSync, cpSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { marked } from 'marked';

const SITE = 'https://glassdesk.dev';
const docs = readFileSync('docs.md', 'utf8');

// Hosts check this to offer updates.
const version = readFileSync('../Cargo.toml', 'utf8').match(/^version = "(.+)"/m)[1];
writeFileSync('public/version.json', JSON.stringify({ version }) + '\n');

// The agent skill, so `curl … /skill.md` installs it.
copyFileSync('../skills/glass/SKILL.md', 'public/skill.md');

// Full docs for LLMs and anyone who wants raw markdown.
writeFileSync('public/llms-full.txt', docs);
writeFileSync('public/docs.md', docs);

// llms.txt: the short index format from https://llmstxt.org.
const summary = docs.match(/^> (.+)$/m)[1];
writeFileSync(
  'public/llms.txt',
  `# Glass

> ${summary}

Install on the computer to be controlled: \`curl -fsSL glassdesk.dev | sh\` (macOS 13+ on Apple Silicon or Intel; Linux x86_64 on Wayland, beta). On a Mac the installer adds Glass.app and opens it for a guided setup (Tailscale, the background service, permissions); in a terminal or over SSH it runs \`glass setup\` instead. From outside the tailnet, \`glass link --anywhere\` makes a one-time link that connects any browser directly over WebRTC (end-to-end encrypted, no open ports). Viewers need only a browser (Safari 26.4+, Chrome, Edge, Firefox). Free and open source (MIT).

Features: every display (live previews along the bottom to switch, or all side by side), touch, trackpad and keyboard (⌘ shortcuts on iPad, per-machine keyboard profiles, a shortcut bar), system audio, clipboard and files both ways, Sharp text (native size, 4:2:2 colour, lossless 4:4:4 once still), up to 120 fps on fast screens and Display P3 colour from wide-colour Mac displays, single-window streaming, a resizable full-screen terminal, forwarded web ports, session recording, chat and drawing on the screen, a privacy screen, Wake-on-LAN, limited links (view-only, no files, no clipboard, time limits, ask first, passkey required), an owner console at \`/admin/\`, an MCP server (\`glass mcp\`) that lets AI agents use the computer while you watch, pause, stop and approve them, a native Mac app (Glass.app) for setup and everything the CLI does, notifications on the viewing device even with Glass closed (Web Push, end-to-end encrypted; answer connection requests and agent questions from the notification), an installable app on every device (badge, per-machine shortcuts, share target, picture in picture, opens offline), single sign-on (OpenID Connect), LDAP, SCIM, access rules, device posture checks, and a self-hostable relay.

## Docs

- [Full documentation](${SITE}/llms-full.txt): everything in one file: install, setup, features, keyboard behaviour on iPad, CLI reference, architecture, security, troubleshooting, FAQ
- [Documentation (HTML)](${SITE}/docs): the same, as a web page
- [Install script](${SITE}/install.sh): what \`curl -fsSL glassdesk.dev | sh\` runs
- [Agent skill](${SITE}/skill.md): how an AI agent sets Glass up on any machine in the user's Tailscale tailnet over SSH (\`glass setup --yes --json\`)
- [Source code](https://github.com/Ntarasiuk/glass): Rust host, browser client, and this website (MIT)

## Optional

- [Feedback API](${SITE}/docs.md#feedback): \`POST ${SITE}/api/feedback\` with JSON \`{"message": "…"}\`
`,
);

// The home page's markdown twin, for agents asking / for text/markdown (src/index.js).
writeFileSync('public/index.md', readFileSync('public/llms.txt', 'utf8'));

// FAQ answers as schema.org Questions (AI answers quote them; no Google rich result since 2026-05).
const faqSection = docs.split(/^## FAQ\n/m)[1]?.split(/^## /m)[0] ?? '';
const faq = [...faqSection.matchAll(/\*\*(.+?\?)\*\* (.+?)(?= \*\*|$)/gm)].map(([, q, a]) => ({
  '@type': 'Question',
  name: q,
  acceptedAnswer: { '@type': 'Answer', text: a.replace(/`/g, '').trim() },
}));
if (faq.length < 5) throw new Error(`docs FAQ: parsed only ${faq.length} questions`);

// HTML docs page.
const title = 'Glass documentation: install, setup and remote access';
const docsDescription =
  'How to install and set up Glass, connect from an iPad or any browser, reach your computer from anywhere, and fix problems. CLI reference and FAQ.';
const docsLd = {
  '@context': 'https://schema.org',
  '@graph': [
    {
      '@type': 'TechArticle',
      '@id': `${SITE}/docs#article`,
      url: `${SITE}/docs`,
      headline: title,
      description: docsDescription,
      about: { '@id': `${SITE}/#software` },
      author: { '@id': `${SITE}/#author` },
      isPartOf: { '@id': `${SITE}/#website` },
      dateModified: lastModified('docs.md'),
      inLanguage: 'en',
    },
    { '@type': 'FAQPage', '@id': `${SITE}/docs#faq`, url: `${SITE}/docs#faq`, mainEntity: faq },
  ],
};
const ogImageAlt = 'Glass: your computer, in any browser. About 20 ms from your Mac or Linux screen to an iPad, phone or laptop.';
const body = marked.parse(docs.replace(/^# Glass\n/, '# Glass documentation\n'));
mkdirSync('public/docs', { recursive: true });
writeFileSync(
  'public/docs/index.html',
  `<!doctype html>
<html lang="en">
<head>
  <!-- Google tag (gtag.js) -->
  <script async src="https://www.googletagmanager.com/gtag/js?id=G-LCYDPMQMLD"></script>
  <script>
    window.dataLayer = window.dataLayer || [];
    function gtag(){dataLayer.push(arguments);}
    gtag('js', new Date());

    gtag('config', 'G-LCYDPMQMLD');
  </script>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>${title}</title>
  <meta name="description" content="${docsDescription}">
  <meta name="robots" content="index, follow, max-image-preview:large, max-snippet:-1">
  <link rel="canonical" href="${SITE}/docs">
  <meta property="og:type" content="article">
  <meta property="og:site_name" content="Glass">
  <meta property="og:url" content="${SITE}/docs">
  <meta property="og:title" content="${title}">
  <meta property="og:description" content="${docsDescription}">
  <meta property="og:image" content="${SITE}/og.png">
  <meta property="og:image:width" content="1200">
  <meta property="og:image:height" content="630">
  <meta property="og:image:alt" content="${ogImageAlt}">
  <meta property="og:video" content="${SITE}/og.mp4">
  <meta property="og:video:secure_url" content="${SITE}/og.mp4">
  <meta property="og:video:type" content="video/mp4">
  <meta property="og:video:width" content="1200">
  <meta property="og:video:height" content="630">
  <meta name="twitter:card" content="summary_large_image">
  <meta name="twitter:image" content="${SITE}/og.png">
  <meta name="twitter:image:alt" content="${ogImageAlt}">
  <script type="application/ld+json">${JSON.stringify(docsLd).replace(/</g, '\\u003c')}</script>
  <link rel="alternate" type="text/markdown" href="/docs.md">
  <link rel="icon" href="/icon.svg" type="image/svg+xml">
  <link rel="preload" href="/fonts/bricolage-grotesque-latin.woff2" as="font" type="font/woff2" crossorigin>
  <link rel="preload" href="/fonts/hanken-grotesk-latin.woff2" as="font" type="font/woff2" crossorigin>
  <link rel="stylesheet" href="/glassy/glassy.css">
  <link rel="stylesheet" href="/style.css">
  <script type="speculationrules">{"prefetch":[{"where":{"href_matches":["/","/features","/docs"]},"eagerness":"moderate"}]}</script>
</head>
<body>
  <div class="wrap">
    <header class="site lg-glass-surface" data-lg-preset="header">
      <a class="brand" href="/"><img src="/icon.svg" alt="" width="26" height="26">Glass</a>
      <nav class="site" aria-label="Main"><a href="/">Home</a><a href="/features">Features</a><a href="/docs.md">Markdown</a><a href="/llms.txt">llms.txt</a></nav>
    </header>
    <main class="doc">
${body}
    </main>
    <footer class="site"><span>Glass. A remote desktop for Mac and Linux.</span><nav aria-label="Footer"><a href="/">Home</a><a href="/features#feedback">Feedback</a></nav></footer>
  </div>
  <script src="/site.js" type="module"></script>
</body>
</html>
`,
);

// Home page: the release its SoftwareApplication describes, and the stylesheets inlined so
// first paint needs only the HTML (about 10 KB compressed, one round trip).
const home = readFileSync('index.html', 'utf8')
  .replace(/"softwareVersion": "[^"]*"/, `"softwareVersion": "${version}"`)
  .replace('<link rel="stylesheet" href="/glassy/glassy.css">', () => `<style>\n${readFileSync('public/glassy/glassy.css', 'utf8')}</style>`)
  .replace('<link rel="stylesheet" href="/style.css">', () => `<style>\n${readFileSync('public/style.css', 'utf8')}</style>`);
if (!home.includes('<style>')) throw new Error('index.html: stylesheet link not found');
writeFileSync('public/index.html', home);

// Sitemap: the HTML pages, each dated by its source's last commit.
const pages = [
  ['/', 'index.html'],
  ['/features', 'public/features/index.html'],
  ['/docs', 'docs.md'],
];
writeFileSync(
  'public/sitemap.xml',
  `<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
${pages.map(([path, src]) => `  <url><loc>${SITE}${path}</loc><lastmod>${lastModified(src)}</lastmod></url>`).join('\n')}
</urlset>
`,
);

// The web client, for remote access from anywhere (glassdesk.dev/app/#r=…).
rmSync('public/app', { recursive: true, force: true });
cpSync('../web', 'public/app', { recursive: true });

// Releases: ../dist (scripts/package.sh, or the signed CI build fetched from GitHub when dist
// lacks this version). "latest" is always this Cargo.toml version, so the installer can't
// serve an older build than version.json announces (hosts would offer the update forever).
mkdirSync('public/releases', { recursive: true });
const dist = '../dist';
const PLATFORMS = ['darwin-universal', 'linux-x86_64'];
const missing = () => PLATFORMS.some((p) => !existsSync(`${dist}/glass-${version}-${p}.tar.gz`));
if (missing()) {
  try {
    execFileSync('gh', ['release', 'download', `v${version}`, '-R', 'Ntarasiuk/glass', '-D', dist, '-p', `glass-${version}-*`, '--clobber'], { stdio: 'inherit' });
  } catch {}
}
// A deploy (npm run deploy) must ship this version; tests and local builds can do without.
const why = `no ${version} release in ${dist}: run scripts/package.sh, or wait for the v${version} release workflow`;
if (missing() && process.env.GLASS_DEPLOY) throw new Error(why);
if (missing()) console.warn(`warning: ${why}`);
// Cloudflare serves assets up to 25 MiB; bigger tarballs stay on the GitHub release, and the
// Worker redirects to them (src/index.js). Their checksums are still served from here.
const ASSET_LIMIT = 25 * 1024 * 1024;
const small = (f) => statSync(f).size < ASSET_LIMIT;
if (existsSync(dist)) {
  for (const f of readdirSync(dist, { withFileTypes: true }).filter((e) => e.isFile() && !e.name.startsWith('glass-latest-')).map((e) => e.name)) {
    if (small(`${dist}/${f}`)) copyFileSync(`${dist}/${f}`, `public/releases/${f}`);
    else rmSync(`public/releases/${f}`, { force: true });
  }
}
// The installer verifies each platform's "latest" tarball against this checksum.
for (const p of PLATFORMS) {
  const release = `${dist}/glass-${version}-${p}.tar.gz`;
  const f = `glass-latest-${p}.tar.gz`;
  if (!existsSync(release)) continue;
  if (small(release)) copyFileSync(release, `public/releases/${f}`);
  else rmSync(`public/releases/${f}`, { force: true });
  // Hash the file itself: with several versions around, any lookup by name can pick a stale one.
  const sum = createHash('sha256').update(readFileSync(release)).digest('hex');
  writeFileSync(`public/releases/${f}.sha256`, `${sum}  ${f}\n`);
}

console.log('built: index.html, skill.md, llms.txt, llms-full.txt, docs.md, docs/index.html, index.md, sitemap.xml, app/, releases/');

/** YYYY-MM-DD of the last commit touching a file (today when uncommitted). */
function lastModified(file) {
  try {
    const date = execFileSync('git', ['log', '-1', '--format=%cs', '--', file], { encoding: 'utf8' }).trim();
    const dirty = execFileSync('git', ['status', '--porcelain', '--', file], { encoding: 'utf8' }).trim();
    if (date && !dirty) return date;
  } catch {}
  return new Date().toISOString().slice(0, 10);
}
