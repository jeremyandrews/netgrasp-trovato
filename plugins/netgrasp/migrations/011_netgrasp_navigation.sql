-- Five entries instead of fifteen: the site menu, the old paths, and Activity.
-- Forward-only; no rollback. Safe to run twice: every statement is guarded.
--
-- docs/UI.md, "Navigation", is the design this implements. In short:
--
--   Home      /overview
--   Devices   /devices     (the Online tab; the other tabs are pages of their own)
--   People    /people
--   Activity  /activity    (Notable; the other tabs are the existing event pages)
--
-- Alerts is the fifth entry, and arrives with the Alerts page (run U3) at
-- weight 3, which is why Activity weighs 4 and not 3.

-- ---------------------------------------------------------------------------
-- The site menu
-- ---------------------------------------------------------------------------
-- The kernel draws one of two menus (`templates/page.html`, `routes/helpers.rs`
-- `inject_site_context`). If `menu_link` holds any row with menu_name 'main' on
-- the live stage it draws those rows, in weight order, and ignores every
-- `tap_menu` entry of every plugin. Otherwise it draws the `tap_menu` entries.
--
-- So these four rows are the whole menu from the next request on, with no
-- restart: they are read on every page. Home, Translate, Post, Blog, Terms,
-- Translate Language and Media leave the bar because the kernel stops drawing
-- the `tap_menu` menu, not because anything was disabled. netgrasp's own
-- `tap_menu` entries now say the same four things, so a site that empties
-- `menu_link` falls back to the same menu plus whatever its other plugins add.
--
-- DO NOTHING on conflict: an operator who renamed or re-weighted one of these
-- in /admin/structure/menus keeps the change when this runs again.
--
-- Two limits of `menu_link` that are the kernel's and not ours: the rows are not
-- filtered by permission, so an anonymous visitor sees all four entries
-- whatever role grants are in place, and the kernel never draws
-- a child row as a child (docs/UI.md, "Why tabs on the page").
INSERT INTO menu_link (menu_name, path, title, weight, hidden, plugin, stage_id, created, changed)
VALUES
    ('main', '/overview', 'Home',     0, false, 'netgrasp', '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint, EXTRACT(EPOCH FROM NOW())::bigint),
    ('main', '/devices',  'Devices',  1, false, 'netgrasp', '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint, EXTRACT(EPOCH FROM NOW())::bigint),
    ('main', '/people',   'People',   2, false, 'netgrasp', '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint, EXTRACT(EPOCH FROM NOW())::bigint),
    ('main', '/activity', 'Activity', 4, false, 'netgrasp', '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint, EXTRACT(EPOCH FROM NOW())::bigint)
ON CONFLICT (path, menu_name, stage_id) DO NOTHING;

-- ---------------------------------------------------------------------------
-- Activity: the notable events
-- ---------------------------------------------------------------------------
-- `ip_changed` is about 95% of every event on a real network (526,962 in seven
-- days on the live site), so a log that shows everything shows nothing else.
-- Notable is what a person would want to be told about: somebody came or went, a
-- device appeared, or one of the three security events a person can act on.
--
-- The list is written here and in netgrasp_core::model::NOTABLE_EVENT_TYPES,
-- and a unit test holds them together.
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'ng_activity',
    'Activity',
    'Comings and goings, new devices, and the security events worth acting on, newest first',
    '{
        "record_type": "ng_event",
        "fields": [],
        "filters": [
            {
                "field": "event_type",
                "operator": "in",
                "value": ["person_arrived", "person_departed", "new_device", "arp_spoof", "ip_conflict", "rogue_dhcp"],
                "exposed": false,
                "exposed_label": null
            }
        ],
        "sorts": [
            {"field": "timestamp", "direction": "desc", "nulls": null}
        ],
        "relationships": [],
        "includes": {}
    }'::jsonb,
    '{
        "format": "table",
        "items_per_page": 50,
        "pager": {"enabled": true, "style": "full", "show_count": true},
        "empty_text": "Nothing notable has happened.",
        "header": null,
        "footer": null,
        "canonical_url": "/activity"
    }'::jsonb,
    'netgrasp',
    EXTRACT(EPOCH FROM NOW())::bigint,
    EXTRACT(EPOCH FROM NOW())::bigint
)
ON CONFLICT (query_id) DO UPDATE SET
    definition = EXCLUDED.definition,
    display    = EXCLUDED.display,
    plugin     = EXCLUDED.plugin,
    changed    = EXCLUDED.changed;

-- ---------------------------------------------------------------------------
-- Paths: /devices is the Online tab, every device is /devices/all
-- ---------------------------------------------------------------------------
-- The menu's Devices entry is /devices, and Devices opens on what is online
-- now. /devices/online keeps answering with the same gather, so a bookmark or
-- a wall screen pointed at it is untouched. The full list moves to
-- /devices/all.
INSERT INTO url_alias (id, source, alias, language, stage_id, created)
VALUES
    (gen_random_uuid(), '/gather/ng_device_online', '/devices',     'en', '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint),
    (gen_random_uuid(), '/gather/ng_device_list',   '/devices/all', 'en', '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint),
    (gen_random_uuid(), '/gather/ng_activity',      '/activity',    'en', '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint)
ON CONFLICT (alias, language, stage_id) DO UPDATE SET source = EXCLUDED.source;

-- Each gather's canonical URL is where its pager and its row menus' "back"
-- links lead (005 explains). The Online tab's is /devices, so paging through
-- what is online keeps the reader on the menu's own entry.
UPDATE gather_query SET
    display = jsonb_set(display, '{canonical_url}', to_jsonb(c.url)),
    changed = EXTRACT(EPOCH FROM NOW())::bigint
FROM (VALUES
    ('ng_device_list',   '/devices/all'),
    ('ng_device_online', '/devices')
) AS c(query_id, url)
WHERE gather_query.query_id = c.query_id;

-- ---------------------------------------------------------------------------
-- 301s: /who-is-home, and /gather/ng_device_list
-- ---------------------------------------------------------------------------
-- The `redirect` table belongs to the trovato_redirects plugin, and the kernel
-- answers from it before any alias is resolved (middleware/redirect.rs), with a
-- real Location header, whenever that plugin is enabled. It is on by default.
--
-- On a brand new database this migration runs BEFORE trovato_redirects' own
-- (`plugin install netgrasp` runs ahead of the first `serve`, which is where the
-- default plugins are migrated), so the table may not exist yet. It is created
-- here with that plugin's DDL, verbatim and IF NOT EXISTS, so that its migration
-- finds it and does nothing. The kernel itself writes this table the same way
-- for every gather's canonical URL (`sync_canonical_redirects`).
CREATE TABLE IF NOT EXISTS redirect (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    source VARCHAR(2048) NOT NULL,
    destination VARCHAR(2048) NOT NULL,
    status_code SMALLINT NOT NULL DEFAULT 301,
    language VARCHAR(12) NOT NULL DEFAULT 'en',
    created BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::bigint
);
CREATE INDEX IF NOT EXISTS idx_redirect_source ON redirect (source);
CREATE INDEX IF NOT EXISTS idx_redirect_language ON redirect (language);

-- Who is home is the People page's Home now tab. The cards it drew are the same
-- people /people/home lists, with their devices one click away.
INSERT INTO redirect (id, source, destination, status_code, language, created)
SELECT gen_random_uuid(), '/who-is-home', '/people/home', 301, 'en', EXTRACT(EPOCH FROM NOW())::bigint
WHERE NOT EXISTS (SELECT 1 FROM redirect WHERE source = '/who-is-home' AND language = 'en');

-- The kernel wrote /gather/ng_device_list -> /devices from the old canonical URL
-- and never rewrites a row it finds, so the move is made here. Only a row still
-- pointing where the kernel pointed it: an operator's own edit is left alone.
UPDATE redirect SET destination = '/devices/all'
WHERE source = '/gather/ng_device_list' AND destination = '/devices';
UPDATE redirect SET destination = '/devices'
WHERE source = '/gather/ng_device_online' AND destination = '/devices/online';
