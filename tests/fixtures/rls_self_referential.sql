-- RLS with self-referential user table pattern
-- Each user can only see and update their own record

CREATE TABLE app_users (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    email TEXT NOT NULL UNIQUE,
    display_name TEXT,
    settings TEXT
);

ALTER TABLE app_users ENABLE ROW LEVEL SECURITY;

-- Users can only select their own record
CREATE POLICY users_self_select ON app_users FOR SELECT
USING (id = current_setting('app.user_id')::uuid);

-- Users can only update their own record
CREATE POLICY users_self_update ON app_users FOR UPDATE
USING (id = current_setting('app.user_id')::uuid);

-- Anyone can insert (for signup), but must set their own id
CREATE POLICY users_insert ON app_users FOR INSERT
WITH CHECK (id = current_setting('app.user_id')::uuid);

-- No DELETE policy = no delete allowed via RLS
