-- New installs and updates, told apart: the installer says which version it replaces.
ALTER TABLE downloads ADD COLUMN via TEXT;       -- 'install' | 'update' (null: before this, or not the installer)
ALTER TABLE downloads ADD COLUMN previous TEXT;  -- the version an update replaces

-- Our own machines, left out of the numbers (salted IP hashes, like downloads.visitor).
CREATE TABLE internal_visitors (
  visitor TEXT PRIMARY KEY,
  label TEXT,
  added INTEGER NOT NULL          -- unix ms
);
