-- Test views translation

CREATE TABLE users (
    id SERIAL PRIMARY KEY,
    username TEXT NOT NULL,
    email TEXT NOT NULL,
    is_active BOOLEAN NOT NULL DEFAULT true
);

CREATE TABLE orders (
    id SERIAL PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id),
    total REAL NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending'
);

-- Simple view
CREATE VIEW active_users AS
SELECT id, username, email
FROM users
WHERE is_active = true;

-- View with join
CREATE VIEW user_orders AS
SELECT u.id AS user_id, u.username, o.id AS order_id, o.total, o.status
FROM users u
JOIN orders o ON u.id = o.user_id;

-- View with aggregation
CREATE VIEW user_order_summary AS
SELECT u.id, u.username, COUNT(o.id) AS order_count, SUM(o.total) AS total_spent
FROM users u
LEFT JOIN orders o ON u.id = o.user_id
GROUP BY u.id, u.username;
