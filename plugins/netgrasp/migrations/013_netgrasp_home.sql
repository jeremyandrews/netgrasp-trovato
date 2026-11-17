-- Home: who is home, today, and what needs attention. And device data only for
-- the people allowed to see it. Forward-only; no rollback. Safe to run twice.
--
-- docs/UI.md, "Home: /overview" and U1 checks 5 and 7.

-- ---------------------------------------------------------------------------
-- What Home counts that the overview did not
-- ---------------------------------------------------------------------------
-- A view over ng_overview (007), adding:
--
--   away_state         'away', the join key for the people who are out, the
--                      way home_state is for the people who are in;
--   devices_unsorted   every device with no name a person gave and no owner,
--                      hidden ones aside: the To sort tab's count, which is
--                      what "N more devices have no owner" is about;
--   arp_spoof_24h, ip_conflict_24h, rogue_dhcp_24h
--                      the three security events a person can act on, over the
--                      last day, one line each on Home when not zero.
--
-- The raw security total stays in ng_overview, unread by Home: on the live
-- network it was 169,785, 98% scan noise, and a number that size says nothing.
--
-- A view over the view rather than columns added to it, because
-- `CREATE OR REPLACE VIEW` cannot drop a column, and 007 must stay re-runnable
-- (012 says the same of the device views). The ng_overview record type is
-- declared over this one now.
CREATE OR REPLACE VIEW ng_home AS
SELECT
    o.*,
    'away'::text AS away_state,
    (SELECT count(*) FROM ng_devices
      WHERE display_name IS NULL AND owner_item_id IS NULL AND NOT hidden)
                                                  AS devices_unsorted,
    (SELECT count(*) FROM ng_events
      WHERE event_type = 'arp_spoof' AND "timestamp" > now() - INTERVAL '24 hours')
                                                  AS arp_spoof_24h,
    (SELECT count(*) FROM ng_events
      WHERE event_type = 'ip_conflict' AND "timestamp" > now() - INTERVAL '24 hours')
                                                  AS ip_conflict_24h,
    (SELECT count(*) FROM ng_events
      WHERE event_type = 'rogue_dhcp' AND "timestamp" > now() - INTERVAL '24 hours')
                                                  AS rogue_dhcp_24h
FROM ng_overview o;

-- ---------------------------------------------------------------------------
-- The page's lists
-- ---------------------------------------------------------------------------
-- Two more includes on the overview gather (008 explains why a page of several
-- lists is one gather with includes):
--
--   away       the people who are out, for one muted line under Home now;
--   attention  new this week and not yet sorted (no name a person gave, no
--              owner, not hidden), newest first, at most six. The rest are a
--              link, never a longer list: on the live network all 166 devices
--              are unsorted.
--
-- `attention` is the `new_devices` definition plus the To sort tab's two
-- conditions, and a test holds it to exactly that. `new_devices` stays as it
-- is, the definition /devices/new is held equal to; Home no longer draws it.
UPDATE gather_query SET
    label = 'Home',
    description = 'Who is home, who came and went today, and what needs attention',
    definition = jsonb_set(jsonb_set(definition, '{includes,away}', '{
        "parent_field": "away_state",
        "child_field": "state",
        "singular": false,
        "definition": {
            "record_type": "ng_person_presence",
            "fields": [],
            "filters": [],
            "sorts": [
                {"field": "name", "direction": "asc", "nulls": null}
            ],
            "relationships": [],
            "includes": {}
        },
        "display": {"items_per_page": 100}
    }'::jsonb), '{includes,attention}', jsonb_build_object(
        'parent_field', 'new_period',
        'child_field', 'period',
        'singular', false,
        'definition', jsonb_set(
            definition #> '{includes,new_devices,definition}',
            '{filters}',
            (definition #> '{includes,new_devices,definition,filters}') || '[
                {"field": "display_name", "operator": "is_null", "value": null, "exposed": false, "exposed_label": null},
                {"field": "owner_id", "operator": "is_null", "value": null, "exposed": false, "exposed_label": null}
            ]'::jsonb
        ),
        'display', '{"items_per_page": 6}'::jsonb
    )),
    changed = EXTRACT(EPOCH FROM NOW())::bigint
WHERE query_id = 'ng_overview';

-- ---------------------------------------------------------------------------
-- Device data is for people who may see it
-- ---------------------------------------------------------------------------
-- A gather has no permission of its own: `view netgrasp devices` gated the menu
-- entries and nothing else, so on the live site `curl /devices/online` with no
-- session returned 100 MAC addresses. The plugin's `tap_field_access` now
-- denies every field of every netgrasp record type, and of its two Item types,
-- to a viewer without that permission; the kernel removes denied fields from
-- every row of a record gather (`apply_record_field_filtering`), includes and
-- all. The pages see the rows arrive empty and say "Sign in" instead.
--
-- The anonymous role held the permission (005, for a localhost dashboard with
-- no login), so the tap alone would change nothing. It is taken away here.
-- A site that does want an open wall screen grants it back, one statement,
-- which the README gives; this runs once, so it will not take it away again.
DELETE FROM role_permissions
WHERE role_id = '00000000-0000-0000-0000-000000000001'
  AND permission = 'view netgrasp devices';
