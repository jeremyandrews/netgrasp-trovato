-- Netgrasp gathers and their routes. Forward-only; no rollback.
--
-- Eight gathers: five over the `ng_device_state` and `ng_event` RECORD types
-- (real Postgres columns, real indexes) and one plain Item gather over
-- `ng_person`. `record_type` in the definition is what makes GatherService
-- resolve the name against the RecordTypeRegistry, query the record's base
-- table, and rewrite the logical filter/sort names through the record field map.
--
-- ===========================================================================
-- Why there is not a single exposed filter in this file
-- ===========================================================================
-- `resolve_exposed_filters` only overwrites a filter's value when the user
-- supplied one, and the query builder emits `field = ''` for Equals rather than
-- skipping it (crates/kernel/src/gather/query_builder.rs). On an Item gather
-- that quietly returns nothing. On a RECORD gather the column is a real
-- Postgres type, and `''` bound against a `uuid` column **raises**:
--
--     invalid input syntax for type uuid: ""
--
-- Netgrasp's facets are owner (uuid) and device (uuid), so a blank exposed
-- filter would 500 the page in its default state. Every facet is therefore its
-- own route with a `{"url_arg": …}` value that is always supplied.
-- G-EXPOSED-FILTER-NO-MATCH-ALL, DESIGN.md Decision 6.

-- ---------------------------------------------------------------------------
-- Devices
-- ---------------------------------------------------------------------------

-- Every device the daemon knows, except the ones an admin hid. Newest sighting
-- first, so the list answers "what is on my network" without being sorted by an
-- id nobody reads.
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'ng_device_list',
    'Devices',
    'Every device seen on the network, most recently seen first',
    '{
        "record_type": "ng_device_state",
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
            {
                "field": "last_seen",
                "direction": "desc",
                "nulls": null
            }
        ],
        "relationships": [],
        "includes": {}
    }'::jsonb,
    '{
        "format": "table",
        "items_per_page": 50,
        "pager": {"enabled": true, "style": "full", "show_count": true},
        "empty_text": "No devices seen yet. Is the netgrasp daemon running?",
        "header": null,
        "footer": null
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

-- What is on the network right now. This is the front page, and it is also the
-- tile whose pager count is "how many devices are online" — a gather is the only
-- way to get a number, because no tile type computes one
-- (G-NO-GATHER-AGGREGATION).
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'ng_device_online',
    'Online devices',
    'Devices currently present on the network',
    '{
        "record_type": "ng_device_state",
        "fields": [],
        "filters": [
            {
                "field": "state",
                "operator": "equals",
                "value": "online",
                "exposed": false,
                "exposed_label": null
            },
            {
                "field": "hidden",
                "operator": "equals",
                "value": false,
                "exposed": false,
                "exposed_label": null
            }
        ],
        "sorts": [
            {
                "field": "last_seen",
                "direction": "desc",
                "nulls": null
            }
        ],
        "relationships": [],
        "includes": {}
    }'::jsonb,
    '{
        "format": "table",
        "items_per_page": 50,
        "pager": {"enabled": true, "style": "full", "show_count": true},
        "empty_text": "Nothing is online right now.",
        "header": null,
        "footer": null
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

-- The device-type facet, as a route rather than an exposed filter.
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'ng_device_by_type',
    'Devices by type',
    'Devices of one type — /devices/type?device_type=phone',
    '{
        "record_type": "ng_device_state",
        "fields": [],
        "filters": [
            {
                "field": "device_type",
                "operator": "equals",
                "value": { "url_arg": "device_type" },
                "exposed": false,
                "exposed_label": null
            }
        ],
        "sorts": [
            {
                "field": "last_seen",
                "direction": "desc",
                "nulls": null
            }
        ],
        "relationships": [],
        "includes": {}
    }'::jsonb,
    '{
        "format": "table",
        "items_per_page": 50,
        "pager": {"enabled": true, "style": "full", "show_count": true},
        "empty_text": "No devices of that type.",
        "header": null,
        "footer": null
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

-- The owner facet: one person's devices. The uuid here is exactly the case that
-- would 500 as a blank exposed filter.
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'ng_device_by_owner',
    'Devices by owner',
    'One person''s devices — /devices/owner?owner=<person item id>',
    '{
        "record_type": "ng_device_state",
        "fields": [],
        "filters": [
            {
                "field": "owner_id",
                "operator": "equals",
                "value": { "url_arg": "owner" },
                "exposed": false,
                "exposed_label": null
            }
        ],
        "sorts": [
            {
                "field": "last_seen",
                "direction": "desc",
                "nulls": null
            }
        ],
        "relationships": [],
        "includes": {}
    }'::jsonb,
    '{
        "format": "table",
        "items_per_page": 50,
        "pager": {"enabled": true, "style": "full", "show_count": true},
        "empty_text": "No devices assigned to that person.",
        "header": null,
        "footer": null
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

-- "Who is home": online devices that belong to somebody. This is the closest a
-- gather gets to a people summary — with no grouping in QueryDefinition, the
-- rows are per-device and a person with three devices appears three times
-- (G-NO-GATHER-AGGREGATION). Sorted by owner so the repetition at least reads as
-- grouping.
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'ng_who_is_home',
    'Who is home',
    'Online devices that belong to a person, grouped by owner',
    '{
        "record_type": "ng_device_state",
        "fields": [],
        "filters": [
            {
                "field": "state",
                "operator": "equals",
                "value": "online",
                "exposed": false,
                "exposed_label": null
            },
            {
                "field": "owner_id",
                "operator": "is_not_null",
                "value": null,
                "exposed": false,
                "exposed_label": null
            },
            {
                "field": "hidden",
                "operator": "equals",
                "value": false,
                "exposed": false,
                "exposed_label": null
            }
        ],
        "sorts": [
            {"field": "owner_id", "direction": "asc", "nulls": null},
            {"field": "last_seen", "direction": "desc", "nulls": null}
        ],
        "relationships": [],
        "includes": {}
    }'::jsonb,
    '{
        "format": "list",
        "items_per_page": 25,
        "pager": {"enabled": true, "style": "full", "show_count": true},
        "empty_text": "Nobody is home.",
        "header": null,
        "footer": null
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
-- Events
-- ---------------------------------------------------------------------------

INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'ng_event_log',
    'Event log',
    'Everything the daemon has noticed, newest first',
    '{
        "record_type": "ng_event",
        "fields": [],
        "filters": [],
        "sorts": [
            {
                "field": "timestamp",
                "direction": "desc",
                "nulls": null
            }
        ],
        "relationships": [],
        "includes": {}
    }'::jsonb,
    '{
        "format": "table",
        "items_per_page": 100,
        "pager": {"enabled": true, "style": "full", "show_count": true},
        "empty_text": "No events recorded.",
        "header": null,
        "footer": null
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

-- The `in` list below holds the daemon's own event-type strings, and only names
-- that `EventType::as_str` can actually produce match anything at all. It is
-- also the daemon's own idea of which of them are security relevant, which is
-- narrower than "anything alarming-sounding": it is the set the daemon's
-- `recent_security_events` selects. See `SECURITY_EVENT_TYPES` for what this
-- list was before a live daemon database was pointed at it.
--
-- Security events get their own route rather than a styled row in the main log,
-- because "styled distinctly via display config" is not something the display
-- JSON can express — it has format/pager/empty_text/header/footer and no
-- per-row conditional styling. A separate route is the honest version of the
-- same intent. The list here must stay in step with
-- netgrasp_core::model::SECURITY_EVENT_TYPES; a unit test asserts it does.
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'ng_event_security',
    'Security events',
    'Scans, spoofs, rogue DHCP, address conflicts and identity changes',
    '{
        "record_type": "ng_event",
        "fields": [],
        "filters": [
            {
                "field": "event_type",
                "operator": "in",
                "value": ["arp_scan", "arp_spoof", "gratuitous_arp", "identity_change", "ip_conflict", "rogue_dhcp"],
                "exposed": false,
                "exposed_label": null
            }
        ],
        "sorts": [
            {
                "field": "timestamp",
                "direction": "desc",
                "nulls": null
            }
        ],
        "relationships": [],
        "includes": {}
    }'::jsonb,
    '{
        "format": "table",
        "items_per_page": 50,
        "pager": {"enabled": true, "style": "full", "show_count": true},
        "empty_text": "Nothing suspicious.",
        "header": null,
        "footer": null
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

-- One device's events. Linked from the device page, which knows the device row
-- id because it looked the row up to render the timelines.
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'ng_event_by_device',
    'Device events',
    'One device''s events — /events/device?device=<device row id>',
    '{
        "record_type": "ng_event",
        "fields": [],
        "filters": [
            {
                "field": "device_id",
                "operator": "equals",
                "value": { "url_arg": "device" },
                "exposed": false,
                "exposed_label": null
            }
        ],
        "sorts": [
            {
                "field": "timestamp",
                "direction": "desc",
                "nulls": null
            }
        ],
        "relationships": [],
        "includes": {}
    }'::jsonb,
    '{
        "format": "table",
        "items_per_page": 100,
        "pager": {"enabled": true, "style": "full", "show_count": true},
        "empty_text": "No events for that device.",
        "header": null,
        "footer": null
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
-- People
-- ---------------------------------------------------------------------------
-- A plain Item gather: people are Items (DESIGN.md Decision 3), so this is the
-- one list in the plugin that reads `item` rather than an ng_ table.
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'ng_person_list',
    'People',
    'People devices can belong to',
    '{
        "base_table": "item",
        "item_type": "ng_person",
        "fields": [],
        "filters": [
            {
                "field": "status",
                "operator": "equals",
                "value": 1,
                "exposed": false,
                "exposed_label": null
            }
        ],
        "sorts": [
            {
                "field": "title",
                "direction": "asc",
                "nulls": null
            }
        ],
        "relationships": [],
        "includes": {}
    }'::jsonb,
    '{
        "format": "list",
        "items_per_page": 50,
        "pager": {"enabled": true, "style": "full", "show_count": true},
        "empty_text": "No people yet. Add one to assign device ownership.",
        "header": null,
        "footer": null
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
-- Human-friendly aliases onto /gather/<query_id>. id is preserved on re-run (it
-- is not in the ON CONFLICT UPDATE SET), Live stage.
INSERT INTO url_alias (id, source, alias, language, stage_id, created)
VALUES
    (gen_random_uuid(), '/gather/ng_device_online',   '/devices/online', 'en', '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint),
    (gen_random_uuid(), '/gather/ng_device_list',     '/devices',        'en', '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint),
    (gen_random_uuid(), '/gather/ng_device_by_type',  '/devices/type',   'en', '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint),
    (gen_random_uuid(), '/gather/ng_device_by_owner', '/devices/owner',  'en', '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint),
    (gen_random_uuid(), '/gather/ng_who_is_home',     '/who-is-home',    'en', '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint),
    (gen_random_uuid(), '/gather/ng_event_log',       '/events',         'en', '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint),
    (gen_random_uuid(), '/gather/ng_event_security',  '/events/security','en', '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint),
    (gen_random_uuid(), '/gather/ng_event_by_device', '/events/device',  'en', '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint),
    (gen_random_uuid(), '/gather/ng_person_list',     '/people',         'en', '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint)
ON CONFLICT (alias, language, stage_id) DO UPDATE SET source = EXCLUDED.source;
