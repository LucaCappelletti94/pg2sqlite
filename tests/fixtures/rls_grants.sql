-- =============================================================================
-- RLS GRANTS: ROLE-BASED ACCESS CONTROL FOR OWNABLES
-- =============================================================================
--
-- This fixture EXTENDS groups.sql and provides a complete ownership/access model:
--
-- Access Hierarchy:
-- ┌─────────┬──────┬──────┬────────┬───────────┬───────────┬───────────┐
-- │ Role    │ View │ Edit │ Delete │ Grant v/e │ Add Admin │ Add Owner │
-- ├─────────┼──────┼──────┼────────┼───────────┼───────────┼───────────┤
-- │ Owner   │  ✓   │  ✓   │   ✓    │     ✓     │     ✓     │     ✓     │
-- │ Admin   │  ✓   │  ✓   │   ✓    │     ✓     │     ✗     │     ✗     │
-- │ Editor  │  ✓   │  ✓   │   ✗    │     ✗     │     ✗     │     ✗     │
-- │ Viewer  │  ✓   │  ✗   │   ✗    │     ✗     │     ✗     │     ✗     │
-- └─────────┴──────┴──────┴────────┴───────────┴───────────┴───────────┘
--
-- Key design decisions:
-- 1. No owner_id column on ownables - ownership is via ownable_owners table
-- 2. created_by is audit metadata, not access control
-- 3. Creator is auto-inserted as first owner via trigger
-- 4. Groups count as 1 entity for "last owner" checks
-- 5. Only owners can add/remove admins and other owners
-- 6. Admins can grant viewer/editor access but cannot manage admins/owners
-- =============================================================================

-- =============================================================================
-- ROLES TABLE (for grant access levels)
-- =============================================================================

CREATE TABLE roles (
    id SMALLINT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
);

-- Insert the grant-level roles (owner/admin are separate tables)
INSERT INTO roles (id, name) VALUES
    (2, 'viewer'),
    (3, 'editor');

-- =============================================================================
-- OWNABLES TABLE
-- =============================================================================

CREATE TABLE ownables (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    title TEXT NOT NULL,
    created_by UUID NOT NULL REFERENCES owners(id) ON DELETE RESTRICT
    -- Note: ON DELETE RESTRICT prevents deleting the creator while ownable exists
    -- This preserves audit trail; to delete creator, first delete/transfer ownables
);

-- =============================================================================
-- OWNABLE OWNERS TABLE
-- Owners have full control: view, edit, delete, grant, add admins, add owners
-- =============================================================================

CREATE TABLE ownable_owners (
    ownable_id UUID NOT NULL REFERENCES ownables(id) ON DELETE CASCADE,
    owner_id UUID NOT NULL REFERENCES owners(id) ON DELETE CASCADE,
    PRIMARY KEY (ownable_id, owner_id)
);

-- Trigger to auto-insert the creator as the first owner.
-- In PostgreSQL, this AFTER INSERT trigger runs after the row is inserted.
-- In SQLite translation, this becomes an AFTER INSERT on the _rls table.
CREATE OR REPLACE FUNCTION ownables_insert_owner()
RETURNS TRIGGER AS $$
BEGIN
    INSERT INTO ownable_owners (ownable_id, owner_id)
    VALUES (NEW.id, NEW.created_by);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER ownables_insert_owner_trigger
AFTER INSERT ON ownables
FOR EACH ROW
EXECUTE FUNCTION ownables_insert_owner();

-- Note: Last owner protection is handled at the application level.
-- The application should check owner_count > 1 before allowing removal.

-- =============================================================================
-- OWNABLE ADMINISTRATORS TABLE
-- Admins have: view, edit, delete, grant viewer/editor
-- Admins cannot: add admins, add owners
-- Tracks who granted admin access for audit purposes
-- =============================================================================

CREATE TABLE ownable_administrators (
    ownable_id UUID NOT NULL REFERENCES ownables(id) ON DELETE CASCADE,
    administrator_id UUID NOT NULL REFERENCES owners(id) ON DELETE CASCADE,
    granted_by UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    PRIMARY KEY (ownable_id, administrator_id)
);

-- =============================================================================
-- GRANTS TABLE
-- For viewer (role_id=2) and editor (role_id=3) access
-- =============================================================================

CREATE TABLE grants (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    ownable_id UUID NOT NULL REFERENCES ownables(id) ON DELETE CASCADE,
    grantee_id UUID NOT NULL REFERENCES owners(id) ON DELETE CASCADE,
    grantor_id UUID NOT NULL REFERENCES owners(id) ON DELETE RESTRICT,
    role_id SMALLINT NOT NULL REFERENCES roles(id),
    UNIQUE (ownable_id, grantee_id)
    -- Note: One grant per grantee per ownable; update role_id to change access level
);

-- =============================================================================
-- NOTE: Access check logic is inlined directly in RLS policies below.
-- This is necessary because PostgreSQL plpgsql functions cannot be directly
-- translated to SQLite. The logic checks:
--
-- has_access_via_membership(user_id, owner_id):
--   user_id = owner_id OR EXISTS(membership from user_id to owner_id as group)
--
-- is_ownable_owner(ownable_id, user_id):
--   EXISTS owner record where has_access_via_membership
--
-- is_ownable_admin(ownable_id, user_id):
--   EXISTS admin record where has_access_via_membership
--
-- has_editor_access: is_owner OR is_admin OR has editor grant
-- has_viewer_access: is_owner OR is_admin OR has any grant
-- can_grant_access: is_owner OR is_admin
-- =============================================================================

-- =============================================================================
-- UNFILTERED COMPANION VIEWS
--
-- A policy consults these instead of the guarded tables themselves. Reading a
-- guarded table applies that table's own policy, so a policy that named one
-- directly would enter another policy, and the four policies here consult each
-- other, which PostgreSQL answers with `infinite recursion detected in policy
-- for relation`. A view runs with its owner's rights, so consulting one asks
-- the question these policies mean to ask, which is who owns or administers a
-- thing, rather than which of those rows the asking user may see.
-- =============================================================================

CREATE VIEW ownables_unfiltered AS
SELECT id, title, created_by FROM ownables;

CREATE VIEW ownable_owners_unfiltered AS
SELECT ownable_id, owner_id FROM ownable_owners;

CREATE VIEW ownable_administrators_unfiltered AS
SELECT ownable_id, administrator_id, granted_by FROM ownable_administrators;

CREATE VIEW grants_unfiltered AS
SELECT id, ownable_id, grantee_id, grantor_id, role_id FROM grants;

-- =============================================================================
-- ROW-LEVEL SECURITY POLICIES FOR OWNABLES
-- =============================================================================

ALTER TABLE ownables ENABLE ROW LEVEL SECURITY;

-- SELECT: Can view if has viewer access (owner, admin, or has any grant)
CREATE POLICY ownables_select_policy ON ownables
FOR SELECT USING (
    -- is_ownable_owner: user is owner directly or via group membership
    EXISTS (
        SELECT 1 FROM ownable_owners_unfiltered oo
        WHERE oo.ownable_id = ownables.id
          AND (oo.owner_id = current_setting('app.user_id')::uuid
               OR EXISTS (SELECT 1 FROM group_memberships gm 
                          WHERE gm.user_id = current_setting('app.user_id')::uuid 
                            AND gm.group_id = oo.owner_id))
    )
    -- is_ownable_admin: user is admin directly or via group membership
    OR EXISTS (
        SELECT 1 FROM ownable_administrators_unfiltered oa
        WHERE oa.ownable_id = ownables.id
          AND (oa.administrator_id = current_setting('app.user_id')::uuid
               OR EXISTS (SELECT 1 FROM group_memberships gm 
                          WHERE gm.user_id = current_setting('app.user_id')::uuid 
                            AND gm.group_id = oa.administrator_id))
    )
    -- has_grant: user has viewer or editor grant directly or via group
    OR EXISTS (
        SELECT 1 FROM grants_unfiltered g
        WHERE g.ownable_id = ownables.id
          AND g.role_id >= 2  -- viewer or higher
          AND (g.grantee_id = current_setting('app.user_id')::uuid
               OR EXISTS (SELECT 1 FROM group_memberships gm 
                          WHERE gm.user_id = current_setting('app.user_id')::uuid 
                            AND gm.group_id = g.grantee_id))
    )
);

-- INSERT: Anyone can create an ownable (they become the owner via trigger)
CREATE POLICY ownables_insert_policy ON ownables
FOR INSERT WITH CHECK (
    created_by = current_setting('app.user_id')::uuid
);

-- UPDATE: Can update if has editor access (owner, admin, or editor grant)
CREATE POLICY ownables_update_policy ON ownables
FOR UPDATE 
USING (
    -- is_ownable_owner
    EXISTS (
        SELECT 1 FROM ownable_owners_unfiltered oo
        WHERE oo.ownable_id = ownables.id
          AND (oo.owner_id = current_setting('app.user_id')::uuid
               OR EXISTS (SELECT 1 FROM group_memberships gm 
                          WHERE gm.user_id = current_setting('app.user_id')::uuid 
                            AND gm.group_id = oo.owner_id))
    )
    -- is_ownable_admin
    OR EXISTS (
        SELECT 1 FROM ownable_administrators_unfiltered oa
        WHERE oa.ownable_id = ownables.id
          AND (oa.administrator_id = current_setting('app.user_id')::uuid
               OR EXISTS (SELECT 1 FROM group_memberships gm 
                          WHERE gm.user_id = current_setting('app.user_id')::uuid 
                            AND gm.group_id = oa.administrator_id))
    )
    -- has_editor_grant
    OR EXISTS (
        SELECT 1 FROM grants_unfiltered g
        WHERE g.ownable_id = ownables.id
          AND g.role_id >= 3  -- editor
          AND (g.grantee_id = current_setting('app.user_id')::uuid
               OR EXISTS (SELECT 1 FROM group_memberships gm 
                          WHERE gm.user_id = current_setting('app.user_id')::uuid 
                            AND gm.group_id = g.grantee_id))
    )
)
WITH CHECK (
    -- Same as USING clause for UPDATE
    EXISTS (
        SELECT 1 FROM ownable_owners_unfiltered oo
        WHERE oo.ownable_id = ownables.id
          AND (oo.owner_id = current_setting('app.user_id')::uuid
               OR EXISTS (SELECT 1 FROM group_memberships gm 
                          WHERE gm.user_id = current_setting('app.user_id')::uuid 
                            AND gm.group_id = oo.owner_id))
    )
    OR EXISTS (
        SELECT 1 FROM ownable_administrators_unfiltered oa
        WHERE oa.ownable_id = ownables.id
          AND (oa.administrator_id = current_setting('app.user_id')::uuid
               OR EXISTS (SELECT 1 FROM group_memberships gm 
                          WHERE gm.user_id = current_setting('app.user_id')::uuid 
                            AND gm.group_id = oa.administrator_id))
    )
    OR EXISTS (
        SELECT 1 FROM grants_unfiltered g
        WHERE g.ownable_id = ownables.id
          AND g.role_id >= 3
          AND (g.grantee_id = current_setting('app.user_id')::uuid
               OR EXISTS (SELECT 1 FROM group_memberships gm 
                          WHERE gm.user_id = current_setting('app.user_id')::uuid 
                            AND gm.group_id = g.grantee_id))
    )
);

-- DELETE: Can delete if owner or admin
CREATE POLICY ownables_delete_policy ON ownables
FOR DELETE USING (
    -- is_ownable_owner
    EXISTS (
        SELECT 1 FROM ownable_owners_unfiltered oo
        WHERE oo.ownable_id = ownables.id
          AND (oo.owner_id = current_setting('app.user_id')::uuid
               OR EXISTS (SELECT 1 FROM group_memberships gm 
                          WHERE gm.user_id = current_setting('app.user_id')::uuid 
                            AND gm.group_id = oo.owner_id))
    )
    -- is_ownable_admin
    OR EXISTS (
        SELECT 1 FROM ownable_administrators_unfiltered oa
        WHERE oa.ownable_id = ownables.id
          AND (oa.administrator_id = current_setting('app.user_id')::uuid
               OR EXISTS (SELECT 1 FROM group_memberships gm 
                          WHERE gm.user_id = current_setting('app.user_id')::uuid 
                            AND gm.group_id = oa.administrator_id))
    )
);

-- =============================================================================
-- ROW-LEVEL SECURITY POLICIES FOR OWNABLE_OWNERS
-- =============================================================================

ALTER TABLE ownable_owners ENABLE ROW LEVEL SECURITY;

-- A read policy cannot consult the table it guards: PostgreSQL answers
-- `infinite recursion detected in policy for relation "ownable_owners"`.
-- A view can, because PostgreSQL runs one with its owner's rights and so
-- bypasses the base table's row level security, which is the standard way to
-- express "any owner of this ownable may see all of its owner rows".

-- SELECT: Can view if has viewer access to the ownable (owner, admin, or grantee)
CREATE POLICY ownable_owners_select_policy ON ownable_owners
FOR SELECT USING (
    -- is_ownable_owner
    EXISTS (
        SELECT 1 FROM ownable_owners_unfiltered oo2
        WHERE oo2.ownable_id = ownable_owners.ownable_id
          AND (oo2.owner_id = current_setting('app.user_id')::uuid
               OR EXISTS (SELECT 1 FROM group_memberships gm 
                          WHERE gm.user_id = current_setting('app.user_id')::uuid 
                            AND gm.group_id = oo2.owner_id))
    )
    -- is_ownable_admin
    OR EXISTS (
        SELECT 1 FROM ownable_administrators_unfiltered oa
        WHERE oa.ownable_id = ownable_owners.ownable_id
          AND (oa.administrator_id = current_setting('app.user_id')::uuid
               OR EXISTS (SELECT 1 FROM group_memberships gm 
                          WHERE gm.user_id = current_setting('app.user_id')::uuid 
                            AND gm.group_id = oa.administrator_id))
    )
    -- has_grant
    OR EXISTS (
        SELECT 1 FROM grants_unfiltered g
        WHERE g.ownable_id = ownable_owners.ownable_id
          AND g.role_id >= 2
          AND (g.grantee_id = current_setting('app.user_id')::uuid
               OR EXISTS (SELECT 1 FROM group_memberships gm 
                          WHERE gm.user_id = current_setting('app.user_id')::uuid 
                            AND gm.group_id = g.grantee_id))
    )
);

-- INSERT: Only owners can add new owners, OR the creator can add themselves as first owner
CREATE POLICY ownable_owners_insert_policy ON ownable_owners
FOR INSERT WITH CHECK (
    -- Allow if user is already an owner (can add more owners)
    EXISTS (
        SELECT 1 FROM ownable_owners_unfiltered oo2
        WHERE oo2.ownable_id = ownable_owners.ownable_id
          AND (oo2.owner_id = current_setting('app.user_id')::uuid
               OR EXISTS (SELECT 1 FROM group_memberships gm 
                          WHERE gm.user_id = current_setting('app.user_id')::uuid 
                            AND gm.group_id = oo2.owner_id))
    )
    -- OR allow inserting yourself as first owner if you're the creator
    OR (
        ownable_owners.owner_id = current_setting('app.user_id')::uuid
        AND NOT EXISTS (
            SELECT 1 FROM ownable_owners_unfiltered oo2
            WHERE oo2.ownable_id = ownable_owners.ownable_id
        )
        AND EXISTS (
            SELECT 1 FROM ownables_unfiltered o
            WHERE o.id = ownable_owners.ownable_id
              AND o.created_by = current_setting('app.user_id')::uuid
        )
    )
);

-- DELETE: Only owners can remove owners
CREATE POLICY ownable_owners_delete_policy ON ownable_owners
FOR DELETE USING (
    EXISTS (
        SELECT 1 FROM ownable_owners_unfiltered oo2
        WHERE oo2.ownable_id = ownable_owners.ownable_id
          AND (oo2.owner_id = current_setting('app.user_id')::uuid
               OR EXISTS (SELECT 1 FROM group_memberships gm 
                          WHERE gm.user_id = current_setting('app.user_id')::uuid 
                            AND gm.group_id = oo2.owner_id))
    )
);

-- =============================================================================
-- ROW-LEVEL SECURITY POLICIES FOR OWNABLE_ADMINISTRATORS
-- =============================================================================

ALTER TABLE ownable_administrators ENABLE ROW LEVEL SECURITY;


-- SELECT: Can view if has viewer access to the ownable
CREATE POLICY ownable_admins_select_policy ON ownable_administrators
FOR SELECT USING (
    -- is_ownable_owner
    EXISTS (
        SELECT 1 FROM ownable_owners_unfiltered oo
        WHERE oo.ownable_id = ownable_administrators.ownable_id
          AND (oo.owner_id = current_setting('app.user_id')::uuid
               OR EXISTS (SELECT 1 FROM group_memberships gm 
                          WHERE gm.user_id = current_setting('app.user_id')::uuid 
                            AND gm.group_id = oo.owner_id))
    )
    -- is_ownable_admin
    OR EXISTS (
        SELECT 1 FROM ownable_administrators_unfiltered oa2
        WHERE oa2.ownable_id = ownable_administrators.ownable_id
          AND (oa2.administrator_id = current_setting('app.user_id')::uuid
               OR EXISTS (SELECT 1 FROM group_memberships gm 
                          WHERE gm.user_id = current_setting('app.user_id')::uuid 
                            AND gm.group_id = oa2.administrator_id))
    )
    -- has_grant
    OR EXISTS (
        SELECT 1 FROM grants_unfiltered g
        WHERE g.ownable_id = ownable_administrators.ownable_id
          AND g.role_id >= 2
          AND (g.grantee_id = current_setting('app.user_id')::uuid
               OR EXISTS (SELECT 1 FROM group_memberships gm 
                          WHERE gm.user_id = current_setting('app.user_id')::uuid 
                            AND gm.group_id = g.grantee_id))
    )
);

-- INSERT: Only owners can add administrators
CREATE POLICY ownable_admins_insert_policy ON ownable_administrators
FOR INSERT WITH CHECK (
    EXISTS (
        SELECT 1 FROM ownable_owners_unfiltered oo
        WHERE oo.ownable_id = ownable_administrators.ownable_id
          AND (oo.owner_id = current_setting('app.user_id')::uuid
               OR EXISTS (SELECT 1 FROM group_memberships gm 
                          WHERE gm.user_id = current_setting('app.user_id')::uuid 
                            AND gm.group_id = oo.owner_id))
    )
);

-- DELETE: Only owners can remove administrators
CREATE POLICY ownable_admins_delete_policy ON ownable_administrators
FOR DELETE USING (
    EXISTS (
        SELECT 1 FROM ownable_owners_unfiltered oo
        WHERE oo.ownable_id = ownable_administrators.ownable_id
          AND (oo.owner_id = current_setting('app.user_id')::uuid
               OR EXISTS (SELECT 1 FROM group_memberships gm 
                          WHERE gm.user_id = current_setting('app.user_id')::uuid 
                            AND gm.group_id = oo.owner_id))
    )
);

-- =============================================================================
-- ROW-LEVEL SECURITY POLICIES FOR GRANTS
-- =============================================================================

ALTER TABLE grants ENABLE ROW LEVEL SECURITY;

-- SELECT: Can view grants you created, grants given to you, or if you're owner/admin
CREATE POLICY grants_select_policy ON grants
FOR SELECT USING (
    -- grantor can see their own grants
    grantor_id = current_setting('app.user_id')::uuid
    -- grantee can see grants given to them (directly or via group)
    OR grantee_id = current_setting('app.user_id')::uuid
    OR EXISTS (SELECT 1 FROM group_memberships gm 
               WHERE gm.user_id = current_setting('app.user_id')::uuid 
                 AND gm.group_id = grants.grantee_id)
    -- owner can see all grants
    OR EXISTS (
        SELECT 1 FROM ownable_owners_unfiltered oo
        WHERE oo.ownable_id = grants.ownable_id
          AND (oo.owner_id = current_setting('app.user_id')::uuid
               OR EXISTS (SELECT 1 FROM group_memberships gm 
                          WHERE gm.user_id = current_setting('app.user_id')::uuid 
                            AND gm.group_id = oo.owner_id))
    )
    -- admin can see all grants
    OR EXISTS (
        SELECT 1 FROM ownable_administrators_unfiltered oa
        WHERE oa.ownable_id = grants.ownable_id
          AND (oa.administrator_id = current_setting('app.user_id')::uuid
               OR EXISTS (SELECT 1 FROM group_memberships gm 
                          WHERE gm.user_id = current_setting('app.user_id')::uuid 
                            AND gm.group_id = oa.administrator_id))
    )
);

-- INSERT: Owners and admins can create grants (grantor must be current user)
CREATE POLICY grants_insert_policy ON grants
FOR INSERT WITH CHECK (
    grantor_id = current_setting('app.user_id')::uuid
    AND (
        -- is_ownable_owner
        EXISTS (
            SELECT 1 FROM ownable_owners_unfiltered oo
            WHERE oo.ownable_id = grants.ownable_id
              AND (oo.owner_id = current_setting('app.user_id')::uuid
                   OR EXISTS (SELECT 1 FROM group_memberships gm 
                              WHERE gm.user_id = current_setting('app.user_id')::uuid 
                                AND gm.group_id = oo.owner_id))
        )
        -- is_ownable_admin
        OR EXISTS (
            SELECT 1 FROM ownable_administrators_unfiltered oa
            WHERE oa.ownable_id = grants.ownable_id
              AND (oa.administrator_id = current_setting('app.user_id')::uuid
                   OR EXISTS (SELECT 1 FROM group_memberships gm 
                              WHERE gm.user_id = current_setting('app.user_id')::uuid 
                                AND gm.group_id = oa.administrator_id))
        )
    )
);

-- UPDATE: Only the grantor can update their grants (e.g., change role)
CREATE POLICY grants_update_policy ON grants
FOR UPDATE 
USING (grantor_id = current_setting('app.user_id')::uuid)
WITH CHECK (grantor_id = current_setting('app.user_id')::uuid);

-- DELETE: Grantor can delete their grants, or owners can delete any grant
CREATE POLICY grants_delete_policy ON grants
FOR DELETE USING (
    grantor_id = current_setting('app.user_id')::uuid
    OR EXISTS (
        SELECT 1 FROM ownable_owners_unfiltered oo
        WHERE oo.ownable_id = grants.ownable_id
          AND (oo.owner_id = current_setting('app.user_id')::uuid
               OR EXISTS (SELECT 1 FROM group_memberships gm 
                          WHERE gm.user_id = current_setting('app.user_id')::uuid 
                            AND gm.group_id = oo.owner_id))
    )
);
