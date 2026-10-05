-- Minimized SQL for triggering recursive logic

CREATE TABLE procedure_template_asset_models (
    id SERIAL PRIMARY KEY,
    name TEXT,
    procedure_template_id INTEGER,
    based_on_id INTEGER,
    asset_model_id INTEGER
);

CREATE TABLE parent_procedure_templates (
    parent_id INTEGER NOT NULL,
    child_id INTEGER NOT NULL,
    PRIMARY KEY (parent_id, child_id)
);

CREATE TABLE next_procedure_templates (
    parent_id INTEGER NOT NULL,
    predecessor_id INTEGER NOT NULL,
    successor_id INTEGER NOT NULL,
    PRIMARY KEY (parent_id, predecessor_id, successor_id)
);

-- Trigger 1: Ensure parent-child relationships exist for sequential steps
CREATE OR REPLACE FUNCTION ensure_parent_procedure_templates() RETURNS TRIGGER LANGUAGE plpgsql AS $$ 
BEGIN 
    INSERT INTO parent_procedure_templates (parent_id, child_id)
    VALUES (NEW.parent_id, NEW.predecessor_id) 
    ON CONFLICT (parent_id, child_id) DO NOTHING;

    INSERT INTO parent_procedure_templates (parent_id, child_id)
    VALUES (NEW.parent_id, NEW.successor_id) 
    ON CONFLICT (parent_id, child_id) DO NOTHING;
    
    RETURN NEW;
END;
$$;

CREATE TRIGGER before_insert_next_procedure_templates 
BEFORE INSERT ON next_procedure_templates 
FOR EACH ROW EXECUTE FUNCTION ensure_parent_procedure_templates();

-- Trigger 2: Propagate asset requirements to parent procedures
CREATE OR REPLACE FUNCTION inherit_procedure_template_asset_models() RETURNS TRIGGER AS $$ 
BEGIN
    INSERT INTO procedure_template_asset_models (
            name,
            procedure_template_id,
            based_on_id,
            asset_model_id
        )
    SELECT 
        pam.name,
        NEW.parent_id,
        pam.id,
        pam.asset_model_id
    FROM procedure_template_asset_models pam
    WHERE pam.procedure_template_id = NEW.child_id;
    
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trg_inherit_procedure_template_asset_models
AFTER INSERT ON parent_procedure_templates 
FOR EACH ROW EXECUTE FUNCTION inherit_procedure_template_asset_models();
