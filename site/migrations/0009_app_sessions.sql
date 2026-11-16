-- The Glass app signed in to an account (/computers). It never has the browser's cookie:
-- its own token, which only lists the account's computers.

-- Sign-ins waiting for a signed-in browser to say yes. The app keeps a secret; the link it
-- opens carries the secret's hash (the code), and only a hash of that is kept here.
CREATE TABLE app_logins (
  code_hash TEXT PRIMARY KEY,
  name TEXT NOT NULL,             -- the computer the app is on, as it says
  account TEXT,                   -- set when a signed-in browser approves
  created INTEGER NOT NULL,
  expires INTEGER NOT NULL,
  visitor TEXT                    -- salted IP hash, for rate limits
);
CREATE INDEX app_logins_expires ON app_logins (expires);
CREATE INDEX app_logins_visitor ON app_logins (visitor, created);

-- Signed-in apps. Only a hash of each token is kept.
CREATE TABLE app_sessions (
  token_hash TEXT PRIMARY KEY,
  account TEXT NOT NULL,
  name TEXT NOT NULL,
  created INTEGER NOT NULL,
  used INTEGER,
  expires INTEGER NOT NULL
);
CREATE INDEX app_sessions_account ON app_sessions (account);
