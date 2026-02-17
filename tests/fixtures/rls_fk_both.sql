-- Test fixture with both tables having RLS
-- Both tables become views with _rls backing tables
-- FKs between them should point to the _rls backing tables

CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    username TEXT NOT NULL UNIQUE,
    email TEXT NOT NULL
);

ALTER TABLE users ENABLE ROW LEVEL SECURITY;
CREATE POLICY users_select ON users FOR SELECT USING (true);
CREATE POLICY users_insert ON users FOR INSERT WITH CHECK (id = current_app_user());

CREATE TABLE posts (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    author_id UUID NOT NULL REFERENCES users(id),
    title TEXT NOT NULL,
    content TEXT
);

ALTER TABLE posts ENABLE ROW LEVEL SECURITY;
CREATE POLICY posts_select ON posts FOR SELECT USING (true);
CREATE POLICY posts_insert ON posts FOR INSERT WITH CHECK (author_id = current_app_user());
CREATE POLICY posts_update ON posts FOR UPDATE USING (author_id = current_app_user());
CREATE POLICY posts_delete ON posts FOR DELETE USING (author_id = current_app_user());
