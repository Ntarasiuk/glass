-- Downloads: one row per install-script or release fetch.
CREATE TABLE downloads (
  id INTEGER PRIMARY KEY,
  ts INTEGER NOT NULL,            -- unix ms
  kind TEXT NOT NULL,             -- 'script' | 'release'
  file TEXT,
  country TEXT,
  user_agent TEXT,
  visitor TEXT                    -- salted hash of the IP; counts unique installs without storing IPs
);
CREATE INDEX downloads_ts ON downloads (ts);
CREATE INDEX downloads_kind_ts ON downloads (kind, ts);

-- Feedback from the website form, `glass feedback`, and the in-app button.
CREATE TABLE feedback (
  id INTEGER PRIMARY KEY,
  ts INTEGER NOT NULL,
  message TEXT NOT NULL,
  email TEXT,
  source TEXT,                    -- 'web' | 'cli' | 'app'
  version TEXT,
  os TEXT,
  country TEXT,
  visitor TEXT
);
CREATE INDEX feedback_ts ON feedback (ts);
CREATE INDEX feedback_visitor_ts ON feedback (visitor, ts);
