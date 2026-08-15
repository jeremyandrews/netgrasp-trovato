-- Netgrasp as a web interface: navigation, landing page, refresh interval.
-- Forward-only; no rollback.

-- ---------------------------------------------------------------------------
-- The anonymous role can see the device pages
-- ---------------------------------------------------------------------------
-- The gathers themselves have always been viewable anonymously — the pages
-- render for a logged-out visitor, and always did. What the anonymous role did
-- not hold was `view netgrasp devices`, the permission every tap_menu entry
-- declares, so the navigation to those same pages was invisible to the only
-- visitor this installation has.
--
-- Two halves had to meet for the menu to appear, and this is the second. The
-- first is in the kernel: `inject_site_context` used to hide any menu entry
-- that declared *any* permission, rather than hiding it from viewers who do not
-- *hold* it, so no grant on any role could have revealed these links. That is
-- fixed in crates/kernel/src/routes/helpers.rs. This grant is what the fixed
-- check now finds.
--
-- The assumption behind granting it to anonymous rather than to a role: this is
-- a localhost home dashboard with no login. To put the whole thing behind a
-- login later, delete this one statement — the `network_viewer` role in
-- 003_netgrasp_roles_tiles.sql already holds the same permission, so an
-- authenticated viewer keeps the navigation without any further change.
INSERT INTO role_permissions (role_id, permission)
VALUES ('00000000-0000-0000-0000-000000000001', 'view netgrasp devices')
ON CONFLICT DO NOTHING;

-- ---------------------------------------------------------------------------
-- Each gather knows its own friendly URL
-- ---------------------------------------------------------------------------
-- 002_netgrasp_gathers.sql aliased nine friendly paths onto /gather/<query_id>,
-- but told the queries nothing about it. The alias is resolved by middleware
-- *before* the handler runs, so a request for /events arrives as
-- /gather/ng_event_log and the handler falls back to that as its `base_path`
-- (crates/kernel/src/routes/gather.rs). Two things follow, and both were
-- visible on the live pages:
--
--   1. Every pager link pointed at /gather/ng_event_log?page=2. It works, and
--      it moves the reader off the URL they arrived on, permanently, at the
--      first click of "Next".
--   2. `current_path` was /gather/ng_event_log, which is not any menu path, so
--      the navigation could never mark the page you were on as active.
--
-- `canonical_url` is the field that already exists for exactly this
-- (QueryDisplay::canonical_url); no gather in this plugin had ever set it. The
-- values below are the aliases from 002, unchanged.
UPDATE gather_query SET
    display = jsonb_set(display, '{canonical_url}', to_jsonb(c.url)),
    changed = EXTRACT(EPOCH FROM NOW())::bigint
FROM (VALUES
    ('ng_device_list',     '/devices'),
    ('ng_device_online',   '/devices/online'),
    ('ng_device_by_type',  '/devices/type'),
    ('ng_device_by_owner', '/devices/owner'),
    ('ng_who_is_home',     '/who-is-home'),
    ('ng_event_log',       '/events'),
    ('ng_event_security',  '/events/security'),
    ('ng_event_by_device', '/events/device'),
    ('ng_person_list',     '/people')
) AS c(query_id, url)
WHERE gather_query.query_id = c.query_id;

-- ---------------------------------------------------------------------------
-- The landing page is the online devices
-- ---------------------------------------------------------------------------
-- `site_front_page` accepts any internal path as of the front-page handler
-- change in crates/kernel/src/routes/front.rs: a value that is not
-- `/item/<uuid>` is served as a redirect. Before that it was read as an item
-- path only, so setting it to a gather route saved cleanly and did nothing.
--
-- DO NOTHING, not DO UPDATE: a plugin migration claiming an unset front page is
-- reasonable for an appliance, and overwriting an operator's choice on every
-- re-run is not.
INSERT INTO site_config (key, value, updated)
VALUES ('site_front_page', '"/devices/online"'::jsonb, NOW())
ON CONFLICT (key) DO NOTHING;

-- ---------------------------------------------------------------------------
-- The auto-reload interval
-- ---------------------------------------------------------------------------
-- One named setting, in seconds, read by every gather page's template through
-- the `refresh_seconds` value the kernel resolves for it. `0` switches the
-- reload off — the template then emits no timer at all rather than arming one
-- it has to cancel — and `?refresh=<seconds>` overrides it for a single page
-- load, which is how it gets tested without editing configuration.
--
-- Ten seconds is also what the templates default to when this row is absent, so
-- deleting it changes nothing; the row exists to be edited.
INSERT INTO site_config (key, value, updated)
VALUES ('gather_refresh_seconds', '10'::jsonb, NOW())
ON CONFLICT (key) DO NOTHING;
