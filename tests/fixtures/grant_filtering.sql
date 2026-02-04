-- Test fixture for grant-based table filtering
-- Tables should be handled differently based on grants to `app_user`:
-- - No grants: Table should not be created in SQLite at all
-- - SELECT only: Table + view created, no write triggers (read-only sync)
-- - SELECT + write grants: Full RLS treatment with INSTEAD OF triggers

-- Create the application role
CREATE ROLE app_user;

-- Table 1: Server-only audit logs (no grants to app_user)
CREATE TABLE audit_logs (
    id UUID PRIMARY KEY,
    event_type TEXT NOT NULL,
    payload TEXT
);
-- No grants to app_user means this table won't be created in SQLite

-- Table 2: Read-only reference table (SELECT only for app_user)
CREATE TABLE users (
    id UUID PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    email TEXT NOT NULL
);
ALTER TABLE users ENABLE ROW LEVEL SECURITY;
CREATE POLICY users_select ON users FOR SELECT TO app_user USING (true);
GRANT SELECT ON users TO app_user;

-- Table 3: User-writable table (full CRUD for app_user)
CREATE TABLE posts (
    id UUID PRIMARY KEY,
    author_id UUID NOT NULL REFERENCES users(id),
    title TEXT NOT NULL,
    content TEXT,
    created_by UUID NOT NULL REFERENCES users(id)
);
ALTER TABLE posts ENABLE ROW LEVEL SECURITY;
CREATE POLICY posts_select ON posts FOR SELECT TO app_user USING (true);
CREATE POLICY posts_insert ON posts FOR INSERT TO app_user WITH CHECK (created_by = current_app_user());
CREATE POLICY posts_update ON posts FOR UPDATE TO app_user USING (created_by = current_app_user());
CREATE POLICY posts_delete ON posts FOR DELETE TO app_user USING (created_by = current_app_user());
GRANT SELECT, INSERT, UPDATE, DELETE ON posts TO app_user;
