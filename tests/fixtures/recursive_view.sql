-- Test for views with recursive CTEs.
-- A simple category hierarchy with a view that flattens the tree.

CREATE TABLE categories (
    id SERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    parent_id INTEGER REFERENCES categories(id)
);

-- View that returns all descendants of each category using WITH RECURSIVE
CREATE VIEW category_tree AS
WITH RECURSIVE tree AS (
    -- Base case: root categories (no parent)
    SELECT id, name, parent_id, 0 AS depth, name AS path
    FROM categories
    WHERE parent_id IS NULL
    UNION ALL
    -- Recursive case: children
    SELECT c.id, c.name, c.parent_id, t.depth + 1, t.path || ' > ' || c.name
    FROM categories c
    JOIN tree t ON c.parent_id = t.id
)
SELECT id, name, parent_id, depth, path FROM tree;

-- View that counts descendants for each category
CREATE VIEW category_descendant_count AS
WITH RECURSIVE descendants AS (
    SELECT id, id AS root_id
    FROM categories
    UNION ALL
    SELECT c.id, d.root_id
    FROM categories c
    JOIN descendants d ON c.parent_id = d.id
)
SELECT root_id AS category_id, COUNT(*) - 1 AS descendant_count
FROM descendants
GROUP BY root_id;
