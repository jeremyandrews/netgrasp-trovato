-- Move three event gathers onto 020's views. Forward-only; no rollback.
--
-- Each update changes `record_type` and nothing a site may have edited: the
-- display (label, pager, page size, empty text) is left alone, and each is
-- guarded on the gather still reading `ng_event`, so it applies once and a
-- site that has pointed one elsewhere keeps its choice. The record types are
-- declared in netgrasp.info.toml, over the views, and sort on `"timestamp"`.
--
-- /activity and /events/new-devices stay on `ng_event`, sorted as before. Their
-- filters name `new_device`, which no daemon index covers, so there is no
-- slice of an index to read: sorting them on `"timestamp"` instead makes the
-- planner walk the time index backwards looking for rare rows, which measured
-- slower than the scan it would replace (Activity: about 250 ms against 160 ms
-- on the live log). They need a daemon index on the event type.

-- /events: the newest 10,000 events.
UPDATE gather_query
SET definition = jsonb_set(definition, '{record_type}', '"ng_event_recent"'),
    changed = EXTRACT(EPOCH FROM NOW())::bigint
WHERE query_id = 'ng_event_log'
  AND definition ->> 'record_type' = 'ng_event';

-- /events/device: one device's newest 10,000.
UPDATE gather_query
SET definition = jsonb_set(definition, '{record_type}', '"ng_device_event"'),
    changed = EXTRACT(EPOCH FROM NOW())::bigint
WHERE query_id = 'ng_event_by_device'
  AND definition ->> 'record_type' = 'ng_event';

-- /events/security: the security view, and no filter of its own. The view is
-- the filter, so the pager's count is `SELECT COUNT(*) FROM
-- ng_security_events`, the same statement the overview's security_events runs.
-- Guarded on the filter being 004's list, so a site that has changed what this
-- page selects keeps that.
UPDATE gather_query
SET definition = jsonb_set(
        jsonb_set(definition, '{record_type}', '"ng_security_event"'),
        '{filters}', '[]'::jsonb),
    changed = EXTRACT(EPOCH FROM NOW())::bigint
WHERE query_id = 'ng_event_security'
  AND definition ->> 'record_type' = 'ng_event'
  AND jsonb_array_length(definition -> 'filters') = 1
  AND definition #>> '{filters,0,field}' = 'event_type'
  AND definition #> '{filters,0,value}' @> '["arp_scan", "arp_spoof", "gratuitous_arp", "identity_change", "ip_conflict", "rogue_dhcp"]'::jsonb
  AND jsonb_array_length(definition #> '{filters,0,value}') = 6;
