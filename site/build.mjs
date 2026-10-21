import { createHash } from 'node:crypto';
// Generates the docs in every format from docs.md (the single source), and copies the
// latest release into public/. Run before `wrangler deploy` (npm run deploy does).

import { copyFileSync, cpSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
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

Install on the computer to be controlled: \`curl -fsSL glassdesk.dev | sh\` (macOS 13+ on Apple Silicon or Intel; Linux x86_64 on Wayland, beta). The installer starts \`glass setup\`, a guided setup for Tailscale, the background service and permissions. From outside the tailnet, \`glass link --anywhere\` makes a one-time link that connects any browser directly over WebRTC (end-to-end encrypted, no open ports). Viewers need only a browser (Safari 26.4+, Chrome, Edge, Firefox). Free and open source (MIT).

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
      <a class="brand" href="/"><img src="/icon.svg" alt="" width="26" height="26">Glass</a>
      <nav class="site" aria-label="Main"><a href="/">Home</a><a href="/docs.md">Markdown</a><a href="/llms.txt">llms.txt</a></nav>
    </header>
    <main class="doc">
${body}
    </main>
    <footer class="site"><span>Glass. A remote desktop for Mac and Linux.</span><nav aria-label="Footer"><a href="/">Home</a><a href="/#feedback">Feedback</a></nav></footer>
  </div>
</body>
</html>
`,
);

// The web client, for remote access from anywhere (glassdesk.dev/app/#r=…).
rmSync('public/app', { recursive: true, force: true });
cpSync('../web', 'public/app', { recursive: true });

// Latest release from the Rust build (../dist, made by scripts/package.sh).
mkdirSync('public/releases', { recursive: true });
const dist = '../dist';
if (existsSync(dist)) {
  for (const f of readdirSync(dist)) copyFileSync(`${dist}/${f}`, `public/releases/${f}`);
}
// The installer verifies each platform's "latest" tarball against this checksum.
const releases = readdirSync('public/releases');
const latest = releases.filter((f) => /^glass-latest-.*\.tar\.gz$/.test(f));
if (!latest.length) console.warn('warning: no release found; run scripts/package.sh first');
for (const f of latest) {
  // Hash the file itself: with several versions around, any lookup by name can pick a stale one.
  const sum = createHash('sha256').update(readFileSync(`public/releases/${f}`)).digest('hex');
  writeFileSync(`public/releases/${f}.sha256`, `${sum}  ${f}\n`);
}

console.log('built: skill.md, llms.txt, llms-full.txt, docs.md, docs/index.html, app/, releases/');
