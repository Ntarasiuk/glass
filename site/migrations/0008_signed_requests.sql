-- What hosts have signed and sent (taking themselves off accounts, fleet reports): each works
-- once, so a copy can't be sent again while its timestamp is still fresh.
CREATE TABLE signed_requests (
  hash TEXT PRIMARY KEY,          -- of the host's id and the exact message it signed
  expires INTEGER NOT NULL        -- unix ms: its timestamp can't pass after this
);
CREATE INDEX signed_requests_expires ON signed_requests (expires);
