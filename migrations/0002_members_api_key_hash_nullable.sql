-- Revoking an API key clears members.api_key_hash, so the column must allow
-- NULL. SQLite cannot drop a NOT NULL constraint in place, so rebuild the
-- table. The DROP below runs with foreign key enforcement on and members is
-- referenced by locations and issues, so it is safe only while those tables
-- hold no rows: every database at this point in the plan is newly migrated
-- and empty, and a database with rows would fail the drop loudly instead of
-- losing data.
CREATE TABLE members_new (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL,
    email TEXT NOT NULL,
    api_key_hash TEXT,
    created_at TEXT NOT NULL,
    UNIQUE (email),
    FOREIGN KEY (account_id) REFERENCES accounts (id)
);
INSERT INTO members_new (id, account_id, email, api_key_hash, created_at)
    SELECT id, account_id, email, api_key_hash, created_at FROM members;
DROP TABLE members;
ALTER TABLE members_new RENAME TO members;
CREATE INDEX idx_members_account_id ON members (account_id);
