-- Trigger with UUID generation and multiple INSERT statements
-- Tests UUID function translation and last_insert_rowid() pattern

CREATE TABLE todos (
    id TEXT PRIMARY KEY DEFAULT gen_random_uuid(),
    title TEXT NOT NULL,
    created_at TEXT
);

CREATE TABLE todo_history (
    id TEXT PRIMARY KEY DEFAULT gen_random_uuid(),
    todo_id TEXT NOT NULL,
    action TEXT NOT NULL,
    created_at TEXT
);

CREATE OR REPLACE FUNCTION log_todo_creation()
RETURNS TRIGGER AS $$
DECLARE
    new_id TEXT;
BEGIN
    new_id := gen_random_uuid();
    INSERT INTO todo_history (id, todo_id, action)
    VALUES (new_id, NEW.id, 'created');
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER todo_creation_trigger
AFTER INSERT ON todos
FOR EACH ROW
EXECUTE FUNCTION log_todo_creation();
