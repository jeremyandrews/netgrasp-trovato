-- Netgrasp as a web interface: navigation and landing page.
-- Forward-only; no rollback.
--
-- Everything below is a row in a kernel table that the kernel already reads.
-- There is no netgrasp-specific code in Trovato behind any of it: the three
-- capabilities this migration leans on (permission-filtered navigation, a front
-- page that accepts any internal path, and a canonical URL per gather) are
-- general kernel features that any plugin can use the same way.

-- ---------------------------------------------------------------------------
-- The anonymous role can see the device pages
-- ---------------------------------------------------------------------------
-- The gathers themselves have always been viewable anonymously — the pages
-- render for a logged-out visitor, and always did. What the anonymous role did
-- not hold was `view netgrasp devices`, the permission every tap_menu entry
-- declares, so the navigation to those same pages was invisible to the only
-- visitor this installation has. The kernel shows a plugin menu entry to a
-- viewer who *holds* the permission it declares (`MenuRegistry::root_menus_for`),
-- so granting it is all that is needed.
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
-- /gather/ng_event_log and the handler falls back to that as its `base_path`.
-- Two things follow, and both were visible on the live pages:
--
--   1. Every pager link pointed at /gather/ng_event_log?page=2. It works, and
--      it moves the reader off the URL they arrived on, permanently, at the
--      first click of "Next".
--   2. `current_path` was /gather/ng_event_log, which is not any menu path, so
--      the navigation could never mark the page you were on as active.
--
-- `canonical_url` is the field that already exists for exactly this
-- (`QueryDisplay::canonical_url`); no gather in this plugin had ever set it. The
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
-- `site_front_page` takes any internal path: the kernel renders `/item/<uuid>`
-- inline and redirects `/` to anything else, so a gather route is a legal front
-- page with no change to the front-page handler.
--
-- DO NOTHING, not DO UPDATE: a plugin migration claiming an unset front page is
-- reasonable for an appliance, and overwriting an operator's choice on every
-- re-run is not. An operator who wants a different landing page sets this key,
-- and re-running the migration leaves their value alone.
--
-- Not in scope for this row: the auto-reload interval. It is not site
-- configuration, because the site context is not injected into a gather content
-- template — it is a literal in templates/gather/netgrasp/page.html, overridable
-- per page load with `?refresh=<seconds>`. That file explains why at length.
INSERT INTO site_config (key, value, updated)
VALUES ('site_front_page', '"/devices/online"'::jsonb, NOW())
ON CONFLICT (key) DO NOTHING;
