-- Test trigger with IF/ELSIF/ELSE blocks

CREATE TABLE audit_log (
    id SERIAL PRIMARY KEY,
    action TEXT NOT NULL,
    severity TEXT NOT NULL
);

CREATE TABLE events (
    id SERIAL PRIMARY KEY,
    event_type TEXT NOT NULL,
    data TEXT
);

CREATE OR REPLACE FUNCTION log_event_trigger()
RETURNS TRIGGER AS $$
BEGIN
    IF NEW.event_type = 'error' THEN
        INSERT INTO audit_log (action, severity) VALUES ('error_event', 'high');
    ELSIF NEW.event_type = 'warning' THEN
        INSERT INTO audit_log (action, severity) VALUES ('warning_event', 'medium');
    ELSIF NEW.event_type = 'info' THEN
        INSERT INTO audit_log (action, severity) VALUES ('info_event', 'low');
    ELSE
        INSERT INTO audit_log (action, severity) VALUES ('unknown_event', 'unknown');
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER event_trigger
AFTER INSERT ON events
FOR EACH ROW
EXECUTE FUNCTION log_event_trigger();
