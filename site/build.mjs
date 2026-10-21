// Generates the docs in every format from docs.md (the single source), and copies the
// latest release into public/. Run before `wrangler deploy` (npm run deploy does).

import { copyFileSync, existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { marked } from 'marked';

const SITE = 'https://getglass.lol';
const docs = readFileSync('docs.md', 'utf8');

// Full docs for LLMs and anyone who wants raw markdown.
writeFileSync('public/llms-full.txt', docs);
writeFileSync('public/docs.md', docs);

// llms.txt: the short index format from https://llmstxt.org.
const summary = docs.match(/^> (.+)$/m)[1];
writeFileSync(
  'public/llms.txt',
  `# Glass

> ${summary}

Install on the Mac to be controlled: \`curl -fsSL getglass.lol | sh\` (macOS 13+, Apple Silicon or Intel). The installer starts \`glass setup\`, a guided setup for Tailscale, the background service and permissions. Viewers need only a browser (Safari 26.4+, Chrome, Edge, Firefox). Free and open source (MIT).

## Docs

- [Full documentation](${SITE}/llms-full.txt): everything in one file: install, setup, features, keyboard behaviour on iPad, CLI reference, architecture, security, troubleshooting, FAQ
- [Documentation (HTML)](${SITE}/docs): the same, as a web page
- [Install script](${SITE}/install.sh): what \`curl -fsSL getglass.lol | sh\` runs
- [Source code](https://github.com/Ntarasiuk/glass): Rust host, browser client, and this website (MIT)

## Optional

- [Feedback API](${SITE}/docs.md#feedback): \`POST ${SITE}/api/feedback\` with JSON \`{"message": "…"}\`
`,
);

// HTML docs page.
const title = 'Glass documentation';
const body = marked.parse(docs.replace(/^# Glass\n/, '# Glass documentation\n'));
mkdirSync('public/docs', { recursive: true });
writeFileSync(
  'public/docs/index.html',
  `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>${title}</title>
  <meta name="description" content="${summary.replace(/"/g, '&quot;')}">
  <link rel="canonical" href="${SITE}/docs">
  <link rel="alternate" type="text/markdown" href="/docs.md">
  <link rel="icon" href="/icon.svg" type="image/svg+xml">
  <link rel="preconnect" href="https://fonts.googleapis.com">
  <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
  <link href="https://fonts.googleapis.com/css2?family=Instrument+Sans:wght@400;600;700&family=JetBrains+Mono:wght@400&display=swap" rel="stylesheet">
  <link rel="stylesheet" href="/style.css">
</head>
<body>
  <div class="wrap">
    <header class="site">
      <a class="brand" href="/"><img src="/icon.svg" alt="" width="28" height="28">Glass</a>
      <nav class="site" aria-label="Main"><a href="/">Home</a><a href="/docs.md">Markdown</a><a href="/llms.txt">llms.txt</a></nav>
    </header>
    <main class="doc">
${body}
    </main>
    <footer class="site"><span>Glass. A remote desktop for Mac.</span><nav aria-label="Footer"><a href="/">Home</a><a href="/#feedback">Feedback</a></nav></footer>
  </div>
</body>
</html>
`,
);

// Latest release from the Rust build (../dist, made by scripts/package.sh).
mkdirSync('public/releases', { recursive: true });
const dist = '../dist';
if (existsSync(dist)) {
  for (const f of readdirSync(dist)) copyFileSync(`${dist}/${f}`, `public/releases/${f}`);
}
const latest = 'public/releases/glass-latest-darwin-universal.tar.gz';
if (!existsSync(latest)) {
  console.warn('warning: no release found; run scripts/package.sh first');
} else {
  // The installer verifies against this checksum.
  const versioned = readdirSync('public/releases').find((f) => /^glass-\d.*\.tar\.gz\.sha256$/.test(f));
  const sum = readFileSync(`public/releases/${versioned}`, 'utf8').split(/\s+/)[0];
  writeFileSync(`${latest}.sha256`, `${sum}  glass-latest-darwin-universal.tar.gz\n`);
}

console.log('built: llms.txt, llms-full.txt, docs.md, docs/index.html, releases/');
