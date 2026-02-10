-- Multi-Tenant Isolation RLS test fixture
-- Users belong to tenants; can only access data within their tenant(s)
-- Demonstrates subquery-based RLS policies with role-based access control

CREATE TABLE tenants (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    name TEXT NOT NULL
);

CREATE TABLE tenant_users (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    tenant_id UUID NOT NULL REFERENCES tenants(id),
    user_id UUID NOT NULL,
    role TEXT NOT NULL DEFAULT 'member'
);

CREATE TABLE projects (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    tenant_id UUID NOT NULL REFERENCES tenants(id),
    name TEXT NOT NULL,
    created_by UUID NOT NULL
);

ALTER TABLE projects ENABLE ROW LEVEL SECURITY;

-- SELECT: User can see projects in any tenant they belong to
CREATE POLICY projects_tenant_select ON projects FOR SELECT
USING (tenant_id IN (
    SELECT tu.tenant_id FROM tenant_users tu
    WHERE tu.user_id = current_setting('app.user_id')::uuid
));

-- INSERT: User can create projects in their tenant(s), must be the creator
CREATE POLICY projects_tenant_insert ON projects FOR INSERT
WITH CHECK (tenant_id IN (
    SELECT tu.tenant_id FROM tenant_users tu
    WHERE tu.user_id = current_setting('app.user_id')::uuid
) AND created_by = current_setting('app.user_id')::uuid);

-- DELETE: Only admins within the tenant can delete projects
CREATE POLICY projects_admin_delete ON projects FOR DELETE
USING (tenant_id IN (
    SELECT tu.tenant_id FROM tenant_users tu
    WHERE tu.user_id = current_setting('app.user_id')::uuid AND tu.role = 'admin'
));
