-- RLS policies using PostgreSQL current_user instead of current_setting()
-- Tests translation of current_user to a mapped SQLite function

CREATE TABLE profiles (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    username TEXT NOT NULL UNIQUE,
    email TEXT NOT NULL,
    is_public BOOLEAN NOT NULL DEFAULT false
);

ALTER TABLE profiles ENABLE ROW LEVEL SECURITY;

-- SELECT: Can see own profile or public profiles
CREATE POLICY profiles_select ON profiles FOR SELECT
USING (username = current_user OR is_public = true);

-- UPDATE: Can only update own profile (USING only, no WITH CHECK)
CREATE POLICY profiles_update ON profiles FOR UPDATE
USING (username = current_user);
