-- =============================================================================
-- GROUPS WITH RECURSIVE MEMBERSHIP PROPAGATION
-- =============================================================================
-- 
-- A simple groups table with recursive triggers for membership propagation.
-- When a user is added to a child group, they are automatically added to
-- all parent groups. When removed from a parent, they are removed from all
-- child groups.
-- =============================================================================

-- Owners table - represents entities who can own items
CREATE TABLE owners (
	id UUID PRIMARY KEY DEFAULT uuidv7()
);

-- Groups table with hierarchy via parent_group_id
CREATE TABLE groups (
    id UUID PRIMARY KEY REFERENCES owners(id) ON DELETE CASCADE,
    parent_group_id UUID REFERENCES groups(id) ON DELETE CASCADE,
    name TEXT NOT NULL
);

-- When a row is inserted in groups, before the insert, we insert the
-- corresponding row in owners to maintain the 1:1 relationship.
CREATE OR REPLACE FUNCTION create_owner_for_group() RETURNS TRIGGER AS $$
BEGIN
	INSERT INTO owners (id) VALUES (NEW.id);
	RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER groups_insert_trigger
BEFORE INSERT ON groups
FOR EACH ROW
EXECUTE FUNCTION create_owner_for_group();

-- Users table
CREATE TABLE users (
    id UUID PRIMARY KEY REFERENCES owners(id) ON DELETE CASCADE,
    name TEXT NOT NULL
);

-- When a row is inserted in users, before the insert, we insert the
-- corresponding row in owners to maintain the 1:1 relationship.
CREATE OR REPLACE FUNCTION create_owner_for_user() RETURNS TRIGGER AS $$
BEGIN
	INSERT INTO owners (id) VALUES (NEW.id);
	RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER users_insert_trigger
BEFORE INSERT ON users
FOR EACH ROW
EXECUTE FUNCTION create_owner_for_user();

-- Junction table for group memberships
CREATE TABLE group_memberships (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    group_id UUID NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    UNIQUE (group_id, user_id)
);

-- When a user is added to a group, also add them to all parent groups
CREATE OR REPLACE FUNCTION add_user_to_parent_groups() RETURNS TRIGGER AS $$
BEGIN
    WITH RECURSIVE parent_groups AS (
        SELECT parent_group_id AS id FROM groups WHERE id = NEW.group_id
        UNION ALL
        SELECT g.parent_group_id FROM groups g JOIN parent_groups pg ON g.id = pg.id
    )
    INSERT INTO group_memberships (group_id, user_id)
    SELECT id, NEW.user_id FROM parent_groups WHERE id IS NOT NULL
    ON CONFLICT (group_id, user_id) DO NOTHING;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER group_membership_insert_trigger
AFTER INSERT ON group_memberships
FOR EACH ROW
EXECUTE FUNCTION add_user_to_parent_groups();

-- When a user is removed from a group, also remove them from all child groups
CREATE OR REPLACE FUNCTION remove_user_from_child_groups() RETURNS TRIGGER AS $$
BEGIN
    WITH RECURSIVE child_groups AS (
        SELECT id FROM groups WHERE parent_group_id = OLD.group_id
        UNION ALL
        SELECT g.id FROM groups g JOIN child_groups cg ON g.parent_group_id = cg.id
    )
    DELETE FROM group_memberships
    WHERE user_id = OLD.user_id
      AND group_id IN (SELECT id FROM child_groups);
    RETURN OLD;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER group_membership_delete_trigger
AFTER DELETE ON group_memberships
FOR EACH ROW
EXECUTE FUNCTION remove_user_from_child_groups();
