// Release file names → what was served. No Worker-only imports, so tests can load it under Node.

/**
 * What a release file is: `glass-0.15.0-darwin-universal.tar.gz`, or `glass-latest-…` (the version in version.json,
 * which is what that request was actually served).
 */
export async function releaseInfo(env, url, file) {
  const m = file.match(/^glass-(latest|\d+\.\d+\.\d+)-([a-z]+)-(\w+)\.(tar\.gz|zip|msi)$/);
  if (!m) return { version: 'unknown', os: 'unknown', arch: 'unknown', format: 'unknown' };
  let version = m[1];
  if (version === 'latest') {
    try {
      version = (await (await env.ASSETS.fetch(new URL('/version.json', url))).json()).version ?? 'latest';
    } catch {
      version = 'latest';
    }
  }
  return { version, os: m[2] === 'darwin' ? 'macos' : m[2], arch: m[3], format: m[4] };
}
