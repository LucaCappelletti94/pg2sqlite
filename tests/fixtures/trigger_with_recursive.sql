-- Trigger with WITH RECURSIVE ... INSERT pattern
-- Tests the transform_with_insert_to_subquery code path

CREATE TABLE tree_nodes (
    id SERIAL PRIMARY KEY,
    parent_id INT,
    name TEXT NOT NULL
);

CREATE TABLE node_ancestors (
    node_id INT NOT NULL,
    ancestor_id INT NOT NULL,
    PRIMARY KEY (node_id, ancestor_id)
);

CREATE OR REPLACE FUNCTION rebuild_ancestors()
RETURNS TRIGGER AS $$
BEGIN
    WITH RECURSIVE chain AS (
        SELECT id, parent_id FROM tree_nodes WHERE id = NEW.id
        UNION ALL
        SELECT c.id, t.parent_id FROM chain c JOIN tree_nodes t ON c.parent_id = t.id WHERE t.parent_id IS NOT NULL
    )
    INSERT INTO node_ancestors (node_id, ancestor_id)
    SELECT NEW.id, parent_id FROM chain WHERE parent_id IS NOT NULL;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER ancestors_trigger
AFTER INSERT ON tree_nodes
FOR EACH ROW
EXECUTE FUNCTION rebuild_ancestors();
