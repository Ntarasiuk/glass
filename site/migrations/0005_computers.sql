-- Computers on accounts (/computers). A computer joins only through a claim its own host
-- signed (`glass account add`), and `glass account remove` takes it off every account.
CREATE TABLE account_computers (
  account TEXT NOT NULL,
  host_id TEXT NOT NULL,          -- the introducer's id for the host
  host_key TEXT NOT NULL,         -- its public key, base64url: what /app/#r= links carry
  name TEXT NOT NULL,
  added INTEGER NOT NULL,         -- unix ms
  PRIMARY KEY (account, host_id)
);
CREATE INDEX account_computers_host ON account_computers (host_id);

-- Claims waiting to be used: a hash of the one-time code in the host's link.
CREATE TABLE computer_claims (
  code_hash TEXT PRIMARY KEY,
  host_id TEXT NOT NULL,
  host_key TEXT NOT NULL,
  name TEXT NOT NULL,
  created INTEGER NOT NULL,
  expires INTEGER NOT NULL,
  visitor TEXT                    -- salted IP hash, for rate limits
);
CREATE INDEX computer_claims_expires ON computer_claims (expires);
CREATE INDEX computer_claims_visitor ON computer_claims (visitor, created);
