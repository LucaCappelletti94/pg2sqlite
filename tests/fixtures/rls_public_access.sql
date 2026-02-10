-- RLS with public and owner-restricted access patterns
-- Tests USING (true) for unconditional access

CREATE TABLE categories (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    name TEXT NOT NULL
);

ALTER TABLE categories ENABLE ROW LEVEL SECURITY;

-- Public read access for categories (anyone can see all categories)
CREATE POLICY categories_public ON categories FOR SELECT USING (true);

-- Only authenticated users can insert (but they can insert any category)
CREATE POLICY categories_insert ON categories FOR INSERT
TO authenticated
WITH CHECK (true);

CREATE TABLE items (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    category_id UUID REFERENCES categories(id),
    name TEXT NOT NULL,
    owner_id UUID NOT NULL,
    is_featured BOOLEAN NOT NULL DEFAULT false
);

ALTER TABLE items ENABLE ROW LEVEL SECURITY;

-- Items are visible if featured OR owned by current user
CREATE POLICY items_select ON items FOR SELECT
USING (is_featured = true OR owner_id = current_setting('app.user_id')::uuid);

-- Only the owner can insert items (and must set themselves as owner)
CREATE POLICY items_insert ON items FOR INSERT
WITH CHECK (owner_id = current_setting('app.user_id')::uuid);

-- Only the owner can update their items
CREATE POLICY items_update ON items FOR UPDATE
USING (owner_id = current_setting('app.user_id')::uuid);

-- Only the owner can delete their items
CREATE POLICY items_delete ON items FOR DELETE
USING (owner_id = current_setting('app.user_id')::uuid);
