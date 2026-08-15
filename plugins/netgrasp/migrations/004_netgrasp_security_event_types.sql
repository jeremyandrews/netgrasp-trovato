-- Correct the security gather's event-type filter. Forward-only; no rollback.
--
-- `002_netgrasp_gathers.sql` seeded `ng_event_security` with an `in` list of
-- five names, four of which the netgrasp daemon never writes: `device_new`,
-- `mac_conflict`, `mac_spoof` and `unknown_device`. The daemon's vocabulary is
-- fixed by `EventType::as_str`, and the two that were meant are spelled
-- `new_device` and `arp_spoof`. Only `ip_conflict` overlapped, so
-- `/events/security` returned exactly the address conflicts and nothing else —
-- a page that renders, pages, and reports a total, and is simply missing most of
-- what it claims to list.
--
-- 002 is corrected too, for installs that have not run yet. This file is for the
-- ones that have: a plugin migration runs once and is recorded in
-- `plugin_migration`, so an edit to an applied file reaches nobody.
--
-- The replacement set is the daemon's own, from `recent_security_events`:
-- arp_scan, arp_spoof, rogue_dhcp, identity_change, ip_conflict, gratuitous_arp.
-- It must stay in step with `netgrasp_core::model::SECURITY_EVENT_TYPES`.
--
-- Written as a targeted `jsonb_set` of the one filter rather than a re-INSERT of
-- the whole definition, so a site that has since edited this gather's display
-- keeps its edits.
UPDATE gather_query
SET definition = jsonb_set(
        definition,
        '{filters,0,value}',
        '["arp_scan", "arp_spoof", "gratuitous_arp", "identity_change", "ip_conflict", "rogue_dhcp"]'::jsonb,
        false
    ),
    description = 'Scans, spoofs, rogue DHCP, address conflicts and identity changes',
    changed = EXTRACT(EPOCH FROM NOW())::bigint
WHERE query_id = 'ng_event_security'
  -- Only the shape 002 seeded: one filter, on event_type, with the stale list.
  -- A site that has already restructured this gather is left alone rather than
  -- having an unrelated filter's value overwritten by path.
  AND jsonb_array_length(definition -> 'filters') = 1
  AND definition #>> '{filters,0,field}' = 'event_type'
  AND definition #> '{filters,0,value}' @> '["mac_spoof"]'::jsonb;
