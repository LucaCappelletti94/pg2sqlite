-- Basic RLS test fixture
-- A simple documents table with row-level security that filters by owner

CREATE TABLE documents (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    owner_id UUID NOT NULL,
    title TEXT NOT NULL,
    content TEXT
);

-- Enable row-level security on the table
ALTER TABLE documents ENABLE ROW LEVEL SECURITY;

-- Create a policy that allows users to see only their own documents
-- Uses current_setting('app.user_id') to get the current user's ID
CREATE POLICY documents_select_policy ON documents
    FOR SELECT
    USING (owner_id = current_setting('app.user_id')::uuid);

-- Create a policy for INSERT that checks the owner_id matches current user
CREATE POLICY documents_insert_policy ON documents
    FOR INSERT
    WITH CHECK (owner_id = current_setting('app.user_id')::uuid);

-- Create a policy for UPDATE that allows updating only own documents
CREATE POLICY documents_update_policy ON documents
    FOR UPDATE
    USING (owner_id = current_setting('app.user_id')::uuid)
    WITH CHECK (owner_id = current_setting('app.user_id')::uuid);

-- Create a policy for DELETE that allows deleting only own documents  
CREATE POLICY documents_delete_policy ON documents
    FOR DELETE
    USING (owner_id = current_setting('app.user_id')::uuid);
