# Agents from anywhere: Glass as a Vercel Connect connector

Status: phases 1–3 built (2026-10-25). Differences from the draft below:

- Access tokens are opaque (`gla_…`), not JWTs: the host is both the authorization server and the resource server, so it looks them up directly, and revoking takes effect at once.
- Public reach is Tailscale Funnel on port 8443 (`glass agents public on`). It points at a separate listener on `127.0.0.1:18443` that serves only the OAuth and MCP routes, so the viewer, console and files are never published. The relay route isn't built.
- Questions to the person don't hold a cloud agent's request open: `confirm` and `request_help` wait 20 s by default (50 s at most over HTTP), then return "pending", and `check_answer` collects the answer later (kept for an hour).
- Not yet built:
  - the agent's own virtual display (`display: "own"`); agents use your screen, with the banner
  - "always allow this for this agent in this app"
  - anomaly pauses beyond the hourly budget
  - the ⌃⌥⌘. physical stop
  - DPoP
  - `jwt-bearer`
  - moving the local `glass mcp` onto grants (it still uses the owner token)

Goal: an agent running anywhere (Vercel, Claude, a server) asks for access to your computer, you approve it with one tap, and from then on it works with no prompts except for things that are truly risky. Glass follows the MCP authorization spec and what Vercel Connect expects of a provider, so `vercel connect create https://<machine>/mcp` just works, as do claude.ai and ChatGPT connectors.

## Principles

1. **The host is the authority.** Each Glass host is its own OAuth authorization server and resource server. glassdesk.dev never holds grants, keys or tokens, so a breach there can't hand anyone your Mac. This keeps Glass account-free.
2. **Ask once, then stay out of the way.** You approve once per agent. After that there are no prompts, only an always-visible indicator, except for actions that are risky.
3. **Enforce on the host, at each input event.** Scopes and rules are checked when input is injected, not trusted from the model.

## Endpoints (on each host)

You reach a host from the internet in one of two ways:

- **Tailscale Funnel** (preferred): `https://<machine>.<tailnet>.ts.net`. Tailscale's certificate, with TLS ending on the host.
- **The Glass relay**: `https://<machine-id>.mcp.glassdesk.dev`. Routed by SNI with no decryption, so TLS still ends on the host.

| Path | Purpose |
|---|---|
| `/mcp` | MCP over streamable HTTP. A request without a token gets a 401 with `WWW-Authenticate: Bearer resource_metadata="…"`. |
| `/.well-known/oauth-protected-resource/mcp` | RFC 9728: `resource`, `authorization_servers: [<host origin>]`, `scopes_supported`. |
| `/.well-known/oauth-authorization-server` | RFC 8414 metadata (below). |
| `/oauth/authorize`, `/oauth/token`, `/oauth/revoke`, `/oauth/register` (+ RFC 7592 `/oauth/register/<id>`), `/oauth/jwks` | |

Authorization server metadata:

```json
{
  "issuer": "https://mac-mini.tail1234.ts.net",
  "authorization_endpoint": ".../oauth/authorize",
  "token_endpoint": ".../oauth/token",
  "revocation_endpoint": ".../oauth/revoke",
  "registration_endpoint": ".../oauth/register",
  "jwks_uri": ".../oauth/jwks",
  "grant_types_supported": ["authorization_code", "refresh_token", "client_credentials"],
  "response_types_supported": ["code"],
  "code_challenge_methods_supported": ["S256"],
  "token_endpoint_auth_methods_supported": ["private_key_jwt", "none", "client_secret_basic"],
  "token_endpoint_auth_signing_alg_values_supported": ["ES256", "RS256"],
  "client_id_metadata_document_supported": true,
  "scopes_supported": ["computer:view", "computer:use", "files", "clipboard"],
  "authorization_details_types_supported": ["glass_computer"]
}
```

Client registration:

- **Client ID metadata documents** are preferred, with DCR (RFC 7591/7592) as a fallback.
- Public clients must use PKCE S256.
- Redirect URIs must match exactly. `https://connect.vercel.com/callback` is just one of them; there is no allowlist of vendors.
- The client's verified **domain**, taken from the CIMD URL or the redirect URI host, is what the consent screen shows. `client_name` and `logo_uri` are only decoration.

## Scopes and grants

| Scope | Allows | Default |
|---|---|---|
| `computer:view` | Screenshots and `wait_for_change` | on |
| `computer:use` | Pointer and keyboard | on |
| `files` | Send and receive files | off |
| `clipboard` | Read and write the clipboard | off |

The scope `*` from Connect means the defaults. Finer limits ride in RFC 9396 `authorization_details` (`type: "glass_computer"`):

- `machines`: this one, or others of yours on the tailnet, reached through `glass mcp`'s existing proxying.
- `display`: `"own"` is a virtual display just for the agent, and is the default. `"main"` is your screen.
- `apps`: bundle ids the agent may control.
- `confirm`: `"risky"` (the default) or `"always"`.

A client that doesn't send these gets the defaults, which you can change on the consent screen.

A grant is stored on the host: the grant id, client domain, scopes, details, created and last-used times, and who approved it.

## Tokens

- **Access tokens** are JWTs signed by the host (ES256) and last **10 minutes**. They always carry `expires_in`, as Connect requires. Claims: `sub` (owner), `client_id`, `aud` (resource), `grant_id`, `scope`, `machines`.
- **The host checks every request against its own grant table**, not just the signature, so revoking takes effect immediately.
- **Refresh tokens:**
  - **Rotated on every use.** Reusing an old one revokes the whole grant, since that means it was stolen.
  - **Expiry:** 30 days of no use, 180 days at most. After that the agent asks again, with one tap.
  - **Confidential clients** (`private_key_jwt`, which Connect prefers) must authenticate to refresh, so a leaked refresh token is useless without the client's key.
- **Revocation (RFC 7009)** is implemented, so revoking in Vercel revokes it on the host too.
- **Too little scope:** a 403 `insufficient_scope` with `WWW-Authenticate: …scope="files"`. The client can then ask for more (MCP step-up), and the user approves only the difference.
- **No DPoP requirement**, because Connect doesn't send DPoP proofs. Short lifetimes plus client-authenticated refresh do that job. Accept DPoP when a client offers it.

## Approving: one tap, no fatigue

1. The agent hits `/mcp` and gets a 401. Connect sends you to `/oauth/authorize`.
2. The consent page, served by the host, shows:
   - the agent's verified domain
   - which computer, with a live thumbnail
   - "Uses its own screen · asks before anything risky"
   - one big **Approve on your phone** button, with the details collapsed below
3. **Number match.** The page shows a 2-digit number, and your owner devices get a push notification: "vercel.app wants to use Mac mini — tap 47". Glass sends that through its existing end-to-end encrypted Web Push, and only owner devices get it. Tapping the right number approves. This stops "just tap Allow" fatigue and scam attacks, because the push alone can't approve. You can also approve on the Mac itself, or with a passkey on the page.
4. The page redirects with a code, and Connect exchanges it.

Who can approve: owner devices only, meaning tailnet devices signed in as the owner, or the Mac app. Never guests, links or quick-support sessions. While a quick-support session is active, consent is refused outright.

**`app` subject** (`client_credentials`, for machine pools such as QA Mac minis):

- A client registered with secret or `private_key_jwt` credentials can't get tokens until the owner or an admin **installs** it, once, from the Mac app or `/admin/`, choosing machines and scopes.
- After that there are no prompts.
- Its own virtual display is mandatory, and it never gets `display: main`.

**`jwt-bearer`**: deferred. When added, it will be for teams whose OIDC issuer an admin trusts explicitly.

## Day to day: what you see

| Moment | Interruption |
|---|---|
| The agent works on its own display | None. A menu bar indicator shows it, and its display appears as a thumbnail in viewers. |
| The agent works on your screen (`display: main`) | The banner with Pause and Stop. Your own mouse or keyboard pauses it until you resume or 10 s pass with no input. |
| A risky action: sending, buying, deleting, posting | A `confirm` card or push. Each one has **Always allow this for this agent in this app** (e.g. "send in Mail"), so the same question isn't asked twice. |
| Off-limits: secure input on, Keychain, Passwords, System Settings privacy panes, `sudo` in a terminal | The action is refused, the agent is told why, and the agent can `request_help`. |
| First run each day | One quiet notification ("Agent X started on Mac mini"), not one per run. |
| Revoke | One tap from any of those notifications, the Mac app, `glass agents revoke`, or Vercel. |

## Enforcement on the host (each input event)

- The scope covers the action, and the grant is live and not paused or stopped.
- The frontmost app is on the grant's `apps` list, checked at injection, not when the step was planned.
- The target display is the agent's own unless the grant allows `main`.
- A step budget per hour; going over it pauses the agent and asks.
- Anything unusual pauses and asks: a new client IP range, a burst of actions, odd hours. This fails closed.
- **Audit log:** append-only and hash-chained, one entry per action (grant, client, action, frontmost app, time). Typed text is redacted. It's viewable in the Mac app and linked to the recording.
- **Physical stop:** pressing ⌃⌥⌘. on the Mac stops every agent.

## Existing local `glass mcp`

The local stdio server keeps working. It becomes a first-party client with a grant created in the Mac app, so local agents get the same scopes, audit and revocation. The host token moves to the Keychain.

## Tests

- **One e2e test per refusal:**
  - out of scope
  - wrong app
  - wrong display
  - expired, revoked or replayed refresh token
  - a token signed by another host
  - consent from a guest or link, or during quick support
  - secure input on
- **Fuzzing** of the token, authorize and register endpoints.
- **Prompt injection:** hostile pages and documents run against real models.
- **An external review** before Funnel or relay access ships.

## Phases

1. Grants, scopes and enforcement on the host, plus the audit log. Local `glass mcp` moves onto grants.
2. The OAuth authorization server, `/mcp` over HTTP, the consent page with number match, and revocation. Tested on the tailnet with Vercel Connect, claude.ai and the MCP Inspector.
3. Public reach: Funnel, then the relay with SNI passthrough.
4. The `app` subject with installs and machine pools, the agent's own virtual display by default, and triggers once Connect documents how inbound signing works.

## Open questions

- Connect's inbound webhook signing and device-code support aren't documented yet.
- Whether Connect forwards `authorization_details` to custom providers. Until it does, the consent screen sets them.
- The relay needs a certificate for `<id>.mcp.glassdesk.dev` issued to the host. That means DNS-01 through glassdesk.dev, and the host should watch Certificate Transparency logs for certificates it didn't request.
