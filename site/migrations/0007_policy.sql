-- The rules an account holds its computers to (FileVault on, a minimum macOS, …). The page
-- checks each computer's report against them; nothing here can change a computer.
CREATE TABLE account_policy (
  account TEXT PRIMARY KEY,
  data TEXT NOT NULL              -- JSON, only the fields cleanPolicy allows
);
