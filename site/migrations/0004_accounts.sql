-- Accounts at glassdesk.dev/computers: a passkey and nothing else (no email, no password).
CREATE TABLE accounts (
  id TEXT PRIMARY KEY,            -- random, base64url; the passkeys' user handle
  created INTEGER NOT NULL        -- unix ms
);

CREATE TABLE passkeys (
  id TEXT PRIMARY KEY,            -- credential id, base64url
  account TEXT NOT NULL,
  public_key TEXT NOT NULL,       -- SubjectPublicKeyInfo, base64url
  alg INTEGER NOT NULL,           -- COSE: -7 ES256, -257 RS256
  sign_count INTEGER NOT NULL DEFAULT 0,
  created INTEGER NOT NULL,
  used INTEGER
);
CREATE INDEX passkeys_account ON passkeys (account);

-- Signed-in browsers. Only a hash of each cookie's token is kept.
CREATE TABLE sessions (
  token_hash TEXT PRIMARY KEY,
  account TEXT NOT NULL,
  created INTEGER NOT NULL,
  expires INTEGER NOT NULL
);
CREATE INDEX sessions_account ON sessions (account);

-- Passkey challenges: each works once, for a few minutes.
CREATE TABLE account_challenges (
  challenge TEXT PRIMARY KEY,
  mode TEXT NOT NULL,             -- 'create' | 'get'
  account TEXT,                   -- create: the account the passkey joins (new or signed in)
  expires INTEGER NOT NULL,
  visitor TEXT                    -- salted IP hash, for rate limits
);
CREATE INDEX account_challenges_visitor ON account_challenges (visitor);
CREATE INDEX account_challenges_expires ON account_challenges (expires);
