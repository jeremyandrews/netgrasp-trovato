-- The overview page, and the three listings it summarises.
-- Forward-only; no rollback.
--
-- /overview answers what somebody glancing at the house wants to know: who is
-- home and since when, who came and went today, what appeared on the network
-- this week, and whether anything suspicious is happening. It replaces
-- /devices/online as the front page (below, and only where that is still the
-- setting).
--
-- ===========================================================================
-- One page, four gathers
-- ===========================================================================
-- A gather page renders one query, and a content template cannot run another
-- (DESIGN.md Decision 10 lists the context it gets). A dashboard tile of type
-- `gather_query` is no way out either: the kernel renders it as an empty
-- `<div data-query-id>` for client script to fill, so it has no rows on the
-- server and nothing without JavaScript.
--
-- What does put several lists into one server render is `includes`: the kernel
-- runs each include as a child gather after the parent, batched, and attaches
-- the child rows to each parent row under the include's name. So the overview
-- is a gather over `ng_overview`, a view with exactly one row whose columns are
-- the counts, and the three lists arrive on that row as includes.
--
-- An include joins by equal values, `child_field IN (the parents' values of
-- parent_field)`, and the kernel binds those values as text. So each join below
-- is text on both sides, and each is a column the child really has: a person's
-- `state`, a movement's `day`, a new device's `period`. 007 says why those
-- columns look the way they do.
--
-- An include's NAME is the key its rows are written under on the parent row,
-- and the kernel inserts it without looking: an include called `people_home`
-- would replace the `people_home` count with a list, and the page would print
-- a list where a number belongs. So the includes are `home`, `movements` and
-- `new_devices`, none of them a column of `ng_overview`, and a test says so.
--
-- Each list is ALSO a gather of its own, with its own route, so that "see all"
-- has somewhere to go and a list can be paged. An include carries its
-- definition inline rather than naming a query, so each definition is written
-- twice: once as the standalone gather and once in the overview. The
-- host-in-the-loop suite asserts the two agree on the record type and on every
-- filter except the one the include's join supplies, which is how a filter
-- added to one copy and not the other gets noticed.
--
-- The sorts differ on purpose in one place: the overview lists today's
-- movements oldest first, because "in order" is the point of a day's arrivals,
-- and /people/movements lists the whole log newest first, because a log read
-- from its oldest row is a log nobody reads.

-- ---------------------------------------------------------------------------
-- The overview
-- ---------------------------------------------------------------------------
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'ng_overview',
    'Overview',
    'Who is home, who came and went today, what is new on the network, and anything suspicious',
    '{
        "record_type": "ng_overview",
        "fields": [],
        "filters": [],
        "sorts": [],
        "relationships": [],
        "includes": {
            "home": {
                "parent_field": "home_state",
                "child_field": "state",
                "singular": false,
                "definition": {
                    "record_type": "ng_person_presence",
                    "fields": [],
                    "filters": [],
                    "sorts": [
                        {"field": "last_arrived", "direction": "asc", "nulls": null},
                        {"field": "name", "direction": "asc", "nulls": null}
                    ],
                    "relationships": [],
                    "includes": {}
                },
                "display": {"items_per_page": 100}
            },
            "movements": {
                "parent_field": "today",
                "child_field": "day",
                "singular": false,
                "definition": {
                    "record_type": "ng_person_movement",
                    "fields": [],
                    "filters": [],
                    "sorts": [
                        {"field": "timestamp", "direction": "asc", "nulls": null}
                    ],
                    "relationships": [],
                    "includes": {}
                },
                "display": {"items_per_page": 100}
            },
            "new_devices": {
                "parent_field": "new_period",
                "child_field": "period",
                "singular": false,
                "definition": {
                    "record_type": "ng_device_new",
                    "fields": [],
                    "filters": [
                        {
                            "field": "hidden",
                            "operator": "equals",
                            "value": false,
                            "exposed": false,
                            "exposed_label": null
                        }
                    ],
                    "sorts": [
                        {"field": "first_seen", "direction": "desc", "nulls": null}
                    ],
                    "relationships": [],
                    "includes": {}
                },
                "display": {"items_per_page": 50}
            }
        }
    }'::jsonb,
    '{
        "format": "list",
        "items_per_page": 1,
        "pager": {"enabled": false, "style": "full", "show_count": false},
        "empty_text": "The overview could not be read.",
        "header": null,
        "footer": null,
        "canonical_url": "/overview"
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
-- Who is home, as people rather than as devices
-- ---------------------------------------------------------------------------
-- /who-is-home is a device listing grouped into cards, because it predates any
-- view over ng_people; it shows WHICH devices make somebody home. This is the
-- person-shaped answer the daemon itself keeps: one row per person, with the
-- time they arrived. Earliest arrival first, so the list reads as the order the
-- house filled up in.
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'ng_people_home',
    'Home now',
    'Everyone the daemon counts as home, and since when',
    '{
        "record_type": "ng_person_presence",
        "fields": [],
        "filters": [
            {
                "field": "state",
                "operator": "equals",
                "value": "home",
                "exposed": false,
                "exposed_label": null
            }
        ],
        "sorts": [
            {"field": "last_arrived", "direction": "asc", "nulls": null},
            {"field": "name", "direction": "asc", "nulls": null}
        ],
        "relationships": [],
        "includes": {}
    }'::jsonb,
    '{
        "format": "list",
        "items_per_page": 50,
        "pager": {"enabled": true, "style": "full", "show_count": true},
        "empty_text": "Nobody is home.",
        "header": null,
        "footer": null,
        "canonical_url": "/people/home"
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
-- Arrivals and departures
-- ---------------------------------------------------------------------------
-- The whole retained log, newest first, or one day of it with ?day=YYYY-MM-DD.
-- The day is a URL argument rather than a fixed filter, and a missing one
-- constrains nothing: the kernel resolves an absent `url_arg` to a null value
-- and drops the filter, so /people/movements with no argument is the log and
-- the overview's "see all" link names today's date.
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'ng_person_movements',
    'Arrivals and departures',
    'Every arrival and departure the daemon recorded, newest first',
    '{
        "record_type": "ng_person_movement",
        "fields": [],
        "filters": [
            {
                "field": "day",
                "operator": "equals",
                "value": { "url_arg": "day" },
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
        "items_per_page": 100,
        "pager": {"enabled": true, "style": "full", "show_count": true},
        "empty_text": "Nobody has arrived or left.",
        "header": null,
        "footer": null,
        "canonical_url": "/people/movements"
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
-- New this week
-- ---------------------------------------------------------------------------
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'ng_devices_new',
    'New this week',
    'Devices first seen in the last seven days, with what the daemon thinks they are',
    '{
        "record_type": "ng_device_new",
        "fields": [],
        "filters": [
            {
                "field": "hidden",
                "operator": "equals",
                "value": false,
                "exposed": false,
                "exposed_label": null
            }
        ],
        "sorts": [
            {"field": "first_seen", "direction": "desc", "nulls": null}
        ],
        "relationships": [],
        "includes": {}
    }'::jsonb,
    '{
        "format": "table",
        "items_per_page": 50,
        "pager": {"enabled": true, "style": "full", "show_count": true},
        "empty_text": "Nothing new has appeared this week.",
        "header": null,
        "footer": null,
        "canonical_url": "/devices/new"
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
-- Routes
-- ---------------------------------------------------------------------------
INSERT INTO url_alias (id, source, alias, language, stage_id, created)
VALUES
    (gen_random_uuid(), '/gather/ng_overview',         '/overview',         'en', '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint),
    (gen_random_uuid(), '/gather/ng_people_home',      '/people/home',      'en', '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint),
    (gen_random_uuid(), '/gather/ng_person_movements', '/people/movements', 'en', '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint),
    (gen_random_uuid(), '/gather/ng_devices_new',      '/devices/new',      'en', '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint)
ON CONFLICT (alias, language, stage_id) DO UPDATE SET source = EXCLUDED.source;

-- ---------------------------------------------------------------------------
-- The front page is the overview
-- ---------------------------------------------------------------------------
-- `/` still redirects: the kernel renders only an `/item/<uuid>` front page
-- inline and sends every other configured path a 307 (routes/front.rs). What
-- changes is where it goes.
--
-- Only where the setting is still the one 005 wrote. 005 claimed an unset key
-- with DO NOTHING so as never to overwrite an operator's choice, and this keeps
-- the same promise from the other side: a site whose front page somebody set to
-- anything else is left exactly as it is.
UPDATE site_config
SET value = '"/overview"'::jsonb,
    updated = NOW()
WHERE key = 'site_front_page'
  AND value = '"/devices/online"'::jsonb;

INSERT INTO site_config (key, value, updated)
VALUES ('site_front_page', '"/overview"'::jsonb, NOW())
ON CONFLICT (key) DO NOTHING;
