-- Test fixture: Vector columns on a table with Row Level Security
-- This tests that vec0 sync triggers correctly reference the backing table (embeddings_rls)
-- instead of the view (embeddings).

CREATE TABLE embeddings (
    id SERIAL PRIMARY KEY,
    content TEXT NOT NULL,
    embedding vector(384)
);

-- Enable RLS on the embeddings table
ALTER TABLE embeddings ENABLE ROW LEVEL SECURITY;

-- Create a permissive policy (allows all operations for testing)
CREATE POLICY embeddings_policy ON embeddings FOR ALL TO PUBLIC USING (true);
