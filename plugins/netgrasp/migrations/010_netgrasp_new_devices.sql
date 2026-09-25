-- New devices, web side: the daemon's new-device events, and the to-do they
-- create. Forward-only; no rollback.
--
-- When a MAC the daemon has never seen appears, it writes one `new_device`
-- event (EventType::NewDevice) and notifies. What the person reading that
-- notification then has to do is always the same: work out what the thing is,
-- give it a name, and say whose it is. Two gathers serve that.
--
-- /events/new-devices is the record: every `new_device` event still inside the
-- retention window, newest first, in the ordinary event table. It answers
-- "what appeared, and when".
--
-- /devices/todo is the task: every device nobody has named or given an owner,
-- newest first. It answers "what is left to do", and a device leaves it the
-- moment either happens, through the row menu on the same page.
--
-- ===========================================================================
-- What "unnamed" means
-- ===========================================================================
-- No `display_name`: no name a person typed. A device the daemon resolved a
-- name for ("jamie-phone" from DHCP, "HP-LaserJet" from mDNS) is still on the
-- list, because a resolved name is the daemon's guess and the task is a person
-- confirming it; the table shows the guess, so confirming it is one rename.
-- Requiring BOTH to be missing, not either, is deliberate too: a device a person
-- has named but not assigned (the router, the printer) is infrastructure that
-- belongs to nobody, and a list that never empties is a list nobody reads.
--
-- Not the event log filtered further. The to-do is about devices as they are
-- now, whatever the log says: a device whose new_device event was pruned at 90
-- days and was never named is exactly as unfinished as one that appeared an hour
-- ago, and a device named since its event is done however recent the event is.
-- Hidden devices are left out, as on every device listing: hiding a device is
-- the other way of saying "dealt with".
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'ng_event_new_devices',
    'New device events',
    'Every device the daemon saw for the first time, newest first',
    '{
        "record_type": "ng_event",
        "fields": [],
        "filters": [
            {
                "field": "event_type",
                "operator": "equals",
                "value": "new_device",
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
        "empty_text": "No new device has appeared.",
        "header": null,
        "footer": null,
        "canonical_url": "/events/new-devices"
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

INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'ng_devices_todo',
    'To do',
    'Devices nobody has named or given an owner yet, newest first',
    '{
        "record_type": "ng_device_state",
        "fields": [],
        "filters": [
            {
                "field": "display_name",
                "operator": "is_null",
                "value": null,
                "exposed": false,
                "exposed_label": null
            },
            {
                "field": "owner_id",
                "operator": "is_null",
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
            {"field": "first_seen", "direction": "desc", "nulls": null}
        ],
        "relationships": [],
        "includes": {}
    }'::jsonb,
    '{
        "format": "table",
        "items_per_page": 50,
        "pager": {"enabled": true, "style": "full", "show_count": true},
        "empty_text": "Every device has a name or an owner. Nothing to do.",
        "header": null,
        "footer": null,
        "canonical_url": "/devices/todo"
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
    (gen_random_uuid(), '/gather/ng_event_new_devices', '/events/new-devices', 'en', '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint),
    (gen_random_uuid(), '/gather/ng_devices_todo',      '/devices/todo',       'en', '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint)
ON CONFLICT (alias, language, stage_id) DO UPDATE SET source = EXCLUDED.source;
