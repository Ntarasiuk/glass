-- What each computer on an account last said about itself (glass fleet on). Only computers
-- that are on an account are kept; a report is dropped with its last account.
CREATE TABLE computer_reports (
  host_id TEXT PRIMARY KEY,
  reported INTEGER NOT NULL,      -- unix ms
  data TEXT NOT NULL              -- JSON, only the fields cleanReport allows
);
