-- Netgrasp auth roles: network_admin (full CRUD) and ng_viewer (read-only).
-- Forward-only migration; no rollback. Kernel tables are guaranteed to exist.

-- network_admin: full CRUD on all ng_* types
INSERT INTO roles (id, name) VALUES (gen_random_uuid(), 'network_admin')
ON CONFLICT (name) DO NOTHING;

INSERT INTO role_permissions (role_id, permission)
SELECT r.id, p.perm
FROM roles r
CROSS JOIN (VALUES
    ('access content'),
    ('view ng_device content'),
    ('create ng_device content'),
    ('edit ng_device content'),
    ('delete ng_device content'),
    ('view ng_person content'),
    ('create ng_person content'),
    ('edit ng_person content'),
    ('delete ng_person content'),
    ('view ng_event content'),
    ('create ng_event content'),
    ('edit ng_event content'),
    ('delete ng_event content'),
    ('view ng_presence content'),
    ('create ng_presence content'),
    ('edit ng_presence content'),
    ('delete ng_presence content'),
    ('view ng_ip_history content'),
    ('create ng_ip_history content'),
    ('edit ng_ip_history content'),
    ('delete ng_ip_history content'),
    ('view ng_location content'),
    ('create ng_location content'),
    ('edit ng_location content'),
    ('delete ng_location content')
) AS p(perm)
WHERE r.name = 'network_admin'
ON CONFLICT (role_id, permission) DO NOTHING;

-- ng_viewer: read-only access
INSERT INTO roles (id, name) VALUES (gen_random_uuid(), 'ng_viewer')
ON CONFLICT (name) DO NOTHING;

INSERT INTO role_permissions (role_id, permission)
SELECT r.id, p.perm
FROM roles r
CROSS JOIN (VALUES
    ('access content')
) AS p(perm)
WHERE r.name = 'ng_viewer'
ON CONFLICT (role_id, permission) DO NOTHING;
