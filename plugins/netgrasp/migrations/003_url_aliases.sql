-- Netgrasp URL aliases: /devices and /events.
-- Forward-only migration; no rollback. Kernel tables are guaranteed to exist.

-- Note: gen_random_uuid() is evaluated on each run but the id column is NOT in
-- the ON CONFLICT UPDATE SET, so the original UUID is preserved on re-runs.

INSERT INTO url_alias (id, source, alias, language, stage_id, created)
VALUES (
    gen_random_uuid(),
    '/gather/ng_device_list',
    '/devices',
    'en',
    'live',
    EXTRACT(EPOCH FROM NOW())::bigint
)
ON CONFLICT (alias, language, stage_id) DO UPDATE SET
    source = EXCLUDED.source;

INSERT INTO url_alias (id, source, alias, language, stage_id, created)
VALUES (
    gen_random_uuid(),
    '/gather/ng_event_log',
    '/events',
    'en',
    'live',
    EXTRACT(EPOCH FROM NOW())::bigint
)
ON CONFLICT (alias, language, stage_id) DO UPDATE SET
    source = EXCLUDED.source;
