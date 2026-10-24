-- Quick support (glass support): a short code standing for a one-time remote link, for an
-- hour. Codes and their cancel tokens are kept only as salted hashes; a code works once.
CREATE TABLE support_codes (
  code_hash TEXT PRIMARY KEY,
  url TEXT NOT NULL,
  cancel_hash TEXT NOT NULL,
  created INTEGER NOT NULL,       -- unix ms
  expires INTEGER NOT NULL,       -- unix ms
  visitor TEXT                    -- who registered it (salted IP hash), for rate limits
);
CREATE INDEX support_codes_expires ON support_codes (expires);
CREATE INDEX support_codes_visitor ON support_codes (visitor, created);
CREATE INDEX support_codes_cancel ON support_codes (cancel_hash);

-- Wrong codes typed at /help, so guessing is slow.
CREATE TABLE support_misses (
  visitor TEXT NOT NULL,
  ts INTEGER NOT NULL
);
CREATE INDEX support_misses_visitor_ts ON support_misses (visitor, ts);
