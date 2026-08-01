-- Netgrasp roles, permissions and dashboard tiles. Forward-only; no rollback.

-- ---------------------------------------------------------------------------
-- Roles
-- ---------------------------------------------------------------------------
-- The scope names these "network_admin" and "viewer". The second is seeded as
-- `network_viewer` instead: `roles.name` is a site-wide unique namespace shared
-- with every other plugin and with whatever the site operator has created, and a
-- role called plain `viewer` is the kind of name two plugins collide on. The
-- permission strings are unchanged.
--
-- Worth knowing before relying on network_admin: the *admin screens*
-- (/admin/content/...) are gated on the users.is_admin flag, not on permissions
-- (crates/kernel/src/routes/helpers.rs, require_admin). So this role grants
-- device and person management through the JSON item routes, which do check
-- permissions, and not through the admin UI. G-ADMIN-UI-IS-ADMIN-ONLY, inherited
-- unchanged from Argus M3.
INSERT INTO roles (id, name)
VALUES
    ('019a4730-0000-7000-8000-0000000000c1', 'network_admin'),
    ('019a4730-0000-7000-8000-0000000000c2', 'network_viewer')
ON CONFLICT (name) DO NOTHING;

-- Permission strings match what tap_perm declares:
-- PermissionDefinition::crud_for_type emits "<verb> <type> content".
INSERT INTO role_permissions (role_id, permission)
SELECT r.id, p.permission
FROM roles r
CROSS JOIN (VALUES
    ('administer netgrasp'),
    ('view netgrasp devices'),
    ('view ng_device content'),
    ('create ng_device content'),
    ('edit ng_device content'),
    ('delete ng_device content'),
    ('view ng_person content'),
    ('create ng_person content'),
    ('edit ng_person content'),
    ('delete ng_person content')
) AS p(permission)
WHERE r.name = 'network_admin'
ON CONFLICT DO NOTHING;

-- Read-only. Deliberately holds no `edit`/`create`/`delete` on either type, so
-- the JSON item routes refuse a write from a viewer — that refusal is what the
-- permission-gating test asserts.
INSERT INTO role_permissions (role_id, permission)
SELECT r.id, p.permission
FROM roles r
CROSS JOIN (VALUES
    ('view netgrasp devices'),
    ('view ng_device content'),
    ('view ng_person content')
) AS p(permission)
WHERE r.name = 'network_viewer'
ON CONFLICT DO NOTHING;

-- ---------------------------------------------------------------------------
-- Tiles
-- ---------------------------------------------------------------------------
-- Four tiles, all of type gather_query, because none of the four tile types
-- (custom_html, menu, gather_query, chat) computes anything and QueryDefinition
-- has no aggregate projection. "How many devices are online" is therefore the
-- pager count of a gather tile rather than a number a tile produced.
-- G-NO-GATHER-AGGREGATION, DESIGN.md Decision 7.
--
-- Keyed on the primary key rather than machine_name: the unique index the tile
-- migration declares on (machine_name, stage_id) is not present on a migrated
-- database, so it is not a usable conflict target.
INSERT INTO tile (id, machine_name, label, region, tile_type, config, weight, status, created, changed)
VALUES
    (
        '019a4730-0000-7000-8000-0000000000d1',
        'ng_online_now',
        'Online now',
        'sidebar_first',
        'gather_query',
        '{"query_id": "ng_device_online"}'::jsonb,
        0,
        1,
        EXTRACT(EPOCH FROM NOW())::bigint,
        EXTRACT(EPOCH FROM NOW())::bigint
    ),
    (
        '019a4730-0000-7000-8000-0000000000d2',
        'ng_who_is_home',
        'Who is home',
        'sidebar_first',
        'gather_query',
        '{"query_id": "ng_who_is_home"}'::jsonb,
        1,
        1,
        EXTRACT(EPOCH FROM NOW())::bigint,
        EXTRACT(EPOCH FROM NOW())::bigint
    ),
    (
        '019a4730-0000-7000-8000-0000000000d3',
        'ng_security_alerts',
        'Security alerts',
        'sidebar_first',
        'gather_query',
        '{"query_id": "ng_event_security"}'::jsonb,
        2,
        1,
        EXTRACT(EPOCH FROM NOW())::bigint,
        EXTRACT(EPOCH FROM NOW())::bigint
    ),
    (
        '019a4730-0000-7000-8000-0000000000d4',
        'ng_recent_events',
        'Recent events',
        'sidebar_second',
        'gather_query',
        '{"query_id": "ng_event_log"}'::jsonb,
        0,
        1,
        EXTRACT(EPOCH FROM NOW())::bigint,
        EXTRACT(EPOCH FROM NOW())::bigint
    )
ON CONFLICT (id) DO UPDATE SET
    machine_name = EXCLUDED.machine_name,
    label     = EXCLUDED.label,
    region    = EXCLUDED.region,
    tile_type = EXCLUDED.tile_type,
    config    = EXCLUDED.config,
    weight    = EXCLUDED.weight,
    changed   = EXCLUDED.changed;
