-- Multi-session-variable RLS fixture.
--
-- This fixture exercises a policy that simultaneously references:
--   * current_setting('app.user_id')  — mapped to a binary UUID function
--   * current_user                    — mapped to a text username function
--
-- SELECT: a user can read messages they sent OR messages addressed to them.
-- INSERT: a user can only create messages where sender_id equals their own UUID.

CREATE TABLE messages (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    sender_id UUID NOT NULL,
    recipient_username TEXT NOT NULL,
    content TEXT NOT NULL
);

ALTER TABLE messages ENABLE ROW LEVEL SECURITY;

-- SELECT policy: visible if you are the sender (by UUID) OR the recipient (by username)
CREATE POLICY messages_select ON messages
    FOR SELECT
    USING (
        sender_id = current_setting('app.user_id')::uuid
        OR recipient_username = current_user
    );

-- INSERT policy: can only send messages as yourself (UUID check)
CREATE POLICY messages_insert ON messages
    FOR INSERT
    WITH CHECK (
        sender_id = current_setting('app.user_id')::uuid
    );
