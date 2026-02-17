-- Netgrasp gather queries: device list and event log.
-- Forward-only migration; no rollback. Kernel tables are guaranteed to exist.

-- ng_device_list: table format, 50/page, 3 exposed filters, sort by display_name asc
-- NOTE: owner_id filter uses "contains" (LIKE) on a RecordReference (UUID) field.
-- "contains" with empty default produces LIKE '%%' = match all (correct no-op).
-- "equals" with empty default would produce = '' = match nothing (broken).
-- Substring match on UUIDs is semantically imprecise but is the only operator
-- where an empty exposed default gives correct "show all when unset" behavior.
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'ng_device_list',
    'Devices',
    'Network devices tracked by Netgrasp',
    '{
        "base_table": "item",
        "item_type": "ng_device",
        "fields": [],
        "filters": [
            {
                "field": "status",
                "operator": "equals",
                "value": 1,
                "exposed": false,
                "exposed_label": null
            },
            {
                "field": "fields.state",
                "operator": "contains",
                "value": "",
                "exposed": true,
                "exposed_label": "State"
            },
            {
                "field": "fields.device_type",
                "operator": "contains",
                "value": "",
                "exposed": true,
                "exposed_label": "Device Type"
            },
            {
                "field": "fields.owner_id",
                "operator": "contains",
                "value": "",
                "exposed": true,
                "exposed_label": "Owner"
            }
        ],
        "sorts": [
            {
                "field": "fields.display_name",
                "direction": "asc",
                "nulls": null
            }
        ],
        "relationships": [],
        "includes": {}
    }'::jsonb,
    '{
        "format": "table",
        "items_per_page": 50,
        "pager": {
            "enabled": true,
            "style": "full",
            "show_count": true
        },
        "empty_text": "No devices found.",
        "header": null,
        "footer": null
    }'::jsonb,
    'netgrasp',
    EXTRACT(EPOCH FROM NOW())::bigint,
    EXTRACT(EPOCH FROM NOW())::bigint
)
ON CONFLICT (query_id) DO UPDATE SET
    definition = EXCLUDED.definition,
    display = EXCLUDED.display,
    plugin = EXCLUDED.plugin,
    changed = EXCLUDED.changed;

-- ng_event_log: table format, 100/page, 2 exposed time-range filters, sort by timestamp desc
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'ng_event_log',
    'Event Log',
    'Network events and activity log',
    '{
        "base_table": "item",
        "item_type": "ng_event",
        "fields": [],
        "filters": [
            {
                "field": "status",
                "operator": "equals",
                "value": 1,
                "exposed": false,
                "exposed_label": null
            },
            {
                "field": "fields.timestamp",
                "operator": "greater_or_equal",
                "value": 0,
                "exposed": true,
                "exposed_label": "After"
            },
            {
                "field": "fields.timestamp",
                "operator": "less_or_equal",
                "value": 4102444800,
                "exposed": true,
                "exposed_label": "Before"
            }
        ],
        "sorts": [
            {
                "field": "fields.timestamp",
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
        "pager": {
            "enabled": true,
            "style": "full",
            "show_count": true
        },
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
    display = EXCLUDED.display,
    plugin = EXCLUDED.plugin,
    changed = EXCLUDED.changed;
