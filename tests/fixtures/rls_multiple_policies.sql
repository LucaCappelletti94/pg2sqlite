-- RLS with multiple permissive policies per operation
-- Tests that multiple policies on same operation combine with OR

CREATE TABLE documents (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    owner_id UUID NOT NULL,
    is_public BOOLEAN NOT NULL DEFAULT false,
    department TEXT,
    title TEXT NOT NULL
);

ALTER TABLE documents ENABLE ROW LEVEL SECURITY;

-- Policy 1: Owner can see their own documents
CREATE POLICY docs_owner ON documents FOR SELECT
USING (owner_id = current_setting('app.user_id')::uuid);

-- Policy 2: Anyone can see public documents
CREATE POLICY docs_public ON documents FOR SELECT
USING (is_public = true);

-- Policy 3: Users can see documents in their department
CREATE POLICY docs_dept ON documents FOR SELECT
USING (department = current_setting('app.user_department', true));

-- Insert policy: must be the owner
CREATE POLICY docs_insert ON documents FOR INSERT
WITH CHECK (owner_id = current_setting('app.user_id')::uuid);

-- Update policy: only owner can update
CREATE POLICY docs_update ON documents FOR UPDATE
USING (owner_id = current_setting('app.user_id')::uuid);

-- Delete policy: only owner can delete
CREATE POLICY docs_delete ON documents FOR DELETE
USING (owner_id = current_setting('app.user_id')::uuid);
