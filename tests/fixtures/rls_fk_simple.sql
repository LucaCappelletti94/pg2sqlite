-- Test fixture for foreign key from non-RLS table to RLS table
-- This reproduces the core issue: FKs pointing to views instead of backing tables

CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    username TEXT NOT NULL UNIQUE,
    email TEXT NOT NULL
);

ALTER TABLE users ENABLE ROW LEVEL SECURITY;
CREATE POLICY users_select ON users FOR SELECT USING (true);
CREATE POLICY users_insert ON users FOR INSERT WITH CHECK (id = current_app_user());

-- Non-RLS table with FK to RLS table
-- The FK should be updated to point to users_rls, not users (the view)
CREATE TABLE posts (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    author_id UUID NOT NULL REFERENCES users(id),
    title TEXT NOT NULL,
    content TEXT
);
