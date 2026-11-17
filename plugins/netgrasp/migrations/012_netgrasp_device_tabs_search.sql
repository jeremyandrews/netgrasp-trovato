-- Devices: one page with tabs and a search box. Forward-only; no rollback.
-- Safe to run twice: every statement is guarded.
--
-- docs/UI.md, "Devices": the tabs are Online (/devices), All (/devices/all),
-- New (/devices/new), To sort (/devices/todo), Hidden (/devices/hidden), By
-- place (/devices/location) and By type (/devices/type). Each tab is its own
-- gather, the way this plugin has always done facets (DESIGN.md Decision 6),
-- and every one of them takes the same search.

-- ---------------------------------------------------------------------------
-- What the search box searches
-- ---------------------------------------------------------------------------
-- One lower-case string per device: every name it has (the one a person gave,
-- the one the daemon resolved, its hostname, its mDNS name), its MAC, its
-- vendor, its address and its owner's name. A gather filter compares one column,
-- so the search needs one column to compare.
--
-- Two NEW views rather than a column added to the two that exist. The record
-- types used to be declared over ng_devices_with_owner (006) and ng_devices_new
-- (007), and `CREATE OR REPLACE VIEW` may add a column but never drop one: once
-- either view had a search column, re-running 006 or 007, as the plugin's
-- host-in-the-loop suite does before every test, would raise "cannot drop
-- columns from view". A view over each leaves both migrations exactly as
-- re-runnable as they were. The manifest declares the two record types over
-- these instead.
--
-- `v.*` is expanded when the view is created, so a later column added to 006's
-- view does not appear here until this is run again; the manifest maps no such
-- column today.
CREATE OR REPLACE VIEW ng_devices_listed AS
SELECT
    v.*,
    lower(concat_ws(' ',
        v.display_name, v.resolved_name, v.hostname, v.mdns_name,
        v.mac, v.vendor, v.last_ip, v.owner_name
    )) AS search_text
FROM ng_devices_with_owner v;

CREATE OR REPLACE VIEW ng_devices_new_listed AS
SELECT
    n.*,
    lower(concat_ws(' ',
        n.display_name, n.resolved_name, n.hostname, n.mdns_name,
        n.mac, n.vendor, n.last_ip, n.owner_name
    )) AS search_text
FROM ng_devices_new n;

-- ---------------------------------------------------------------------------
-- The Hidden tab
-- ---------------------------------------------------------------------------
-- Every other device list leaves hidden devices out, which is what hiding is
-- for, so without this a device once hidden could only be found again by MAC
-- in the record admin.
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'ng_devices_hidden',
    'Hidden devices',
    'Devices somebody hid from every other list, most recently seen first',
    '{
        "record_type": "ng_device_state",
        "fields": [],
        "filters": [
            {
                "field": "hidden",
                "operator": "equals",
                "value": true,
                "exposed": false,
                "exposed_label": null
            }
        ],
        "sorts": [
            {"field": "last_seen", "direction": "desc", "nulls": null}
        ],
        "relationships": [],
        "includes": {}
    }'::jsonb,
    '{
        "format": "table",
        "items_per_page": 50,
        "pager": {"enabled": true, "style": "full", "show_count": true},
        "empty_text": "Nothing hidden.",
        "header": null,
        "footer": null,
        "canonical_url": "/devices/hidden"
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

INSERT INTO url_alias (id, source, alias, language, stage_id, created)
VALUES
    (gen_random_uuid(), '/gather/ng_devices_hidden', '/devices/hidden', 'en', '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint)
ON CONFLICT (alias, language, stage_id) DO UPDATE SET source = EXCLUDED.source;

-- ---------------------------------------------------------------------------
-- By type: grouped when no type is given
-- ---------------------------------------------------------------------------
-- /devices/type with no `device_type` has always listed every device (an
-- unresolved url_arg filter is dropped). Sorted by type first, it is the By type
-- tab: the template draws one section per type, the way By place draws one per
-- place. Hidden devices leave it like every other tab.
UPDATE gather_query SET
    definition = jsonb_set(definition, '{sorts}', '[
        {"field": "device_type", "direction": "asc",  "nulls": null},
        {"field": "last_seen",   "direction": "desc", "nulls": null}
    ]'::jsonb),
    display = jsonb_set(jsonb_set(display, '{canonical_url}', '"/devices/type"'), '{items_per_page}', '100'),
    changed = EXTRACT(EPOCH FROM NOW())::bigint
WHERE query_id = 'ng_device_by_type';

UPDATE gather_query SET
    definition = jsonb_set(definition, '{filters}', (definition -> 'filters') || '[
        {"field": "hidden", "operator": "equals", "value": false, "exposed": false, "exposed_label": null}
    ]'::jsonb),
    changed = EXTRACT(EPOCH FROM NOW())::bigint
WHERE query_id = 'ng_device_by_type'
  AND NOT (definition -> 'filters') @> '[{"field": "hidden"}]'::jsonb;

-- ---------------------------------------------------------------------------
-- The search filter, on every tab
-- ---------------------------------------------------------------------------
-- One exposed `contains` filter on the search column. The kernel drops an
-- exposed filter the reader left blank (`resolve_exposed_filters`), so an empty
-- box matches every row, and a filled one is an ILIKE '%…%'. The box itself is a
-- plain GET form in the plugin's template, so it needs no form token.
--
-- Appended only where it is not already there, so a second run, or a run after
-- an earlier migration re-wrote the gather, adds exactly one.
UPDATE gather_query SET
    definition = jsonb_set(definition, '{filters}', (definition -> 'filters') || '[
        {"field": "search", "operator": "contains", "value": null, "exposed": true, "exposed_label": "Search"}
    ]'::jsonb),
    changed = EXTRACT(EPOCH FROM NOW())::bigint
WHERE query_id IN (
        'ng_device_online', 'ng_device_list', 'ng_devices_new', 'ng_devices_todo',
        'ng_devices_hidden', 'ng_devices_by_location', 'ng_device_by_type',
        'ng_device_by_owner'
    )
  AND NOT (definition -> 'filters') @> '[{"field": "search"}]'::jsonb;

-- The overview's list of new devices is the /devices/new definition written a
-- second time, and the two are held equal by a test. The include is never
-- given a search (an include takes no exposed values), so the filter is dropped
-- there as unanswered, and the two stay the same definition.
UPDATE gather_query SET
    definition = jsonb_set(definition, '{includes,new_devices,definition,filters}',
        (definition #> '{includes,new_devices,definition,filters}') || '[
        {"field": "search", "operator": "contains", "value": null, "exposed": true, "exposed_label": "Search"}
    ]'::jsonb),
    changed = EXTRACT(EPOCH FROM NOW())::bigint
WHERE query_id = 'ng_overview'
  AND definition #> '{includes,new_devices}' IS NOT NULL
  AND NOT (definition #> '{includes,new_devices,definition,filters}') @> '[{"field": "search"}]'::jsonb;

-- By place, in the words docs/UI.md gives its empty state. The page's own hint
-- goes on to say why (UniFi enrichment, off by default).
UPDATE gather_query SET
    display = jsonb_set(display, '{empty_text}', '"No device has a place."'),
    changed = EXTRACT(EPOCH FROM NOW())::bigint
WHERE query_id = 'ng_devices_by_location';
