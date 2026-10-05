-- Test DROP statement translations

CREATE TABLE users (
    id SERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT NOT NULL
);

CREATE TABLE posts (
    id SERIAL PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id),
    title TEXT NOT NULL
);

-- Create a view
CREATE VIEW all_users AS
SELECT id, name, email FROM users;

-- Create an index
CREATE INDEX idx_users_email ON users (email);

-- Create a trigger function and trigger
CREATE OR REPLACE FUNCTION log_user_changes() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    RETURN NEW;
END;
$$;

CREATE TRIGGER user_change_trigger
AFTER INSERT ON users
FOR EACH ROW EXECUTE FUNCTION log_user_changes();
