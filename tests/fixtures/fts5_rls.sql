-- Test fixture: FTS5 index on a table with Row Level Security
-- This tests that FTS5 triggers correctly reference the backing table (documents_rls)
-- instead of the view (documents).

CREATE TABLE documents (
    id SERIAL PRIMARY KEY,
    title TEXT NOT NULL,
    content TEXT NOT NULL
);

-- Enable RLS on the documents table
ALTER TABLE documents ENABLE ROW LEVEL SECURITY;

-- Create a permissive policy (allows all operations for testing)
CREATE POLICY documents_policy ON documents FOR ALL TO PUBLIC USING (true);

-- Create a GIN index for full-text search (will be translated to FTS5)
CREATE INDEX idx_documents_search ON documents
    USING GIN (to_tsvector('english', title || ' ' || content));
