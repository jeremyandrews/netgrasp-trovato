-- Devices by where they are. Forward-only; no rollback.
--
-- /devices/location lists every device the daemon has placed, grouped under
-- the place. The place is `ng_devices.current_location`, which the daemon's
-- UniFi enrichment resolves from the access point a device is associated with
-- (`current_ap`), and both columns are null on every row when enrichment is
-- off. So on most installs this page is empty, and its empty text says why
-- rather than leaving it to read as a fault.
--
-- A plain gather over the `ng_device_state` record type the device pages
-- already use, which is why this migration adds no view: the owner view from
-- 006 already carries `current_location` and `current_ap`, and "is placed" is
-- `is_not_null`, a filter a gather can hold. Grouping is the template's job, the
-- same way /who-is-home groups by owner: the gather sorts by place so a
-- place's devices arrive adjacent (G-NO-GATHER-AGGREGATION).
--
-- Devices with an access point but no resolved place are not listed: there is
-- no group to put them under, and the device tables show the access point on
-- its own. Hidden devices are left out, as on every other device listing.
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'ng_devices_by_location',
    'Devices by location',
    'Every device the daemon has placed, grouped by where it is',
    '{
        "record_type": "ng_device_state",
        "fields": [],
        "filters": [
            {
                "field": "current_location",
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
            {"field": "current_location", "direction": "asc", "nulls": null},
            {"field": "last_seen", "direction": "desc", "nulls": null}
        ],
        "relationships": [],
        "includes": {}
    }'::jsonb,
    '{
        "format": "table",
        "items_per_page": 100,
        "pager": {"enabled": true, "style": "full", "show_count": true},
        "empty_text": "No device has a location.",
        "header": null,
        "footer": null,
        "canonical_url": "/devices/location"
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
    (gen_random_uuid(), '/gather/ng_devices_by_location', '/devices/location', 'en', '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint)
ON CONFLICT (alias, language, stage_id) DO UPDATE SET source = EXCLUDED.source;
