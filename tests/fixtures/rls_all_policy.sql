-- RLS test fixture using ALL command
-- A simple documents table with a single policy for all operations

CREATE TABLE documents (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    owner_id UUID NOT NULL,
    title TEXT NOT NULL,
    content TEXT
);

-- Enable row-level security on the table
ALTER TABLE documents ENABLE ROW LEVEL SECURITY;

-- Create a single policy for ALL operations (SELECT, INSERT, UPDATE, DELETE)
-- This should be translated the same as individual policies
CREATE POLICY documents_all_policy ON documents
    FOR ALL
    TO authenticated
    USING (owner_id = current_setting('app.user_id')::uuid)
    WITH CHECK (owner_id = current_setting('app.user_id')::uuid);
