-- Test fixture: DELETE...USING with RLS tables
-- This tests that DELETE...USING statements correctly reference RLS backing tables
-- in the converted EXISTS subquery.

CREATE TABLE users (
    id SERIAL PRIMARY KEY,
    username TEXT NOT NULL,
    active BOOLEAN NOT NULL DEFAULT true
);

-- Enable RLS on the users table
ALTER TABLE users ENABLE ROW LEVEL SECURITY;

-- Create a permissive policy (allows all operations for testing)
CREATE POLICY users_policy ON users FOR ALL TO PUBLIC USING (true);

CREATE TABLE posts (
    id SERIAL PRIMARY KEY,
    author_id INTEGER REFERENCES users(id),
    title TEXT NOT NULL,
    content TEXT
);
