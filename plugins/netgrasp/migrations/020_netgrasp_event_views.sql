-- The event pages read the daemon's indexes. Forward-only; no rollback.
--
-- Schema only, like 007 and 019: three views and the overview redefined over
-- one of them. Nothing here writes a kernel table, so CI applies it over the
-- daemon's DDL and the schema test plans the pages' SQL against it. The
-- gathers move onto these views in 021.
--
-- ===========================================================================
-- Why
-- ===========================================================================
-- A record gather runs two statements per page: the rows, `ORDER BY <sort>
-- LIMIT n OFFSET m`, and the pager's total, `SELECT COUNT(*)` over the same
-- relation and filters. Over `ng_events` both read the whole table, which on a
-- site that keeps 90 days is four million rows, 95% of them `ip_changed`:
--
--   * the sort named `timestamp_epoch`, the generated twin the daemon does not
--     index, so every page sorted the table to show its first 100 rows; and
--   * the count counted every row the filter kept, which for /events is all of
--     them and for one busy device on /events/device is nearly all of them.
--
-- The daemon indexes `"timestamp"` (ng_events_time_idx, V1), a device's events
-- by time (ng_events_device_idx, V1) and the security types by time
-- (ng_events_security_idx, V2). The plugin cannot add an index to the daemon's
-- table, so each view below is shaped so that one of those answers it.
--
-- `"timestamp"` is in every view, and is what the record types sort on (the
-- manifest maps their `timestamp` field to it). `timestamp_epoch` is in every
-- view too, and is still what the templates print, so a time reads exactly as
-- it did. No view selects `*`, for 007's reason: a `*` freezes today's column
-- list, and on a V5 daemon it would also carry `person_item_id`, which the
-- plugin's own copy of the table (001) does not have.

-- ---------------------------------------------------------------------------
-- The event log: the newest 10,000 events
-- ---------------------------------------------------------------------------
-- /events lists everything, newest first. The first page reads 100 entries of
-- the time index and the last of its 100 pages reads 10,000, whatever the size
-- of the log. Its pager count is bounded the same way: it counts at most 10,000
-- index entries, and the page says "10000+" when it reaches that, rather than
-- printing a number it did not count. Paging back further than 10,000 events
-- would be 40,000 pages of `ip_changed`; the assistant's events tool answers
-- for any window.
--
-- 10,000 is written here and in the two templates that print the bound
-- (query--ng_event_log.html, query--ng_event_by_device.html); a test holds the
-- three together.
CREATE OR REPLACE VIEW ng_events_recent AS
SELECT e.id, e.device_id, e.event_type, e."timestamp", e.details, e.notified, e.timestamp_epoch
FROM ng_events e
ORDER BY e."timestamp" DESC
LIMIT 10000;

-- ---------------------------------------------------------------------------
-- One device's events: its newest 10,000
-- ---------------------------------------------------------------------------
-- /events/device?device=N filters `device_id = N`. Over this view that filter
-- lands on `d.id`, the device's primary key, so the lateral reads one device's
-- slice of ng_events_device_idx, newest first, and stops at 10,000. The count
-- stops there too: on the live network one device has four million events, and
-- its page counted every one of them.
--
-- `device_id` is the device row's id, which is the same value as the event's
-- (the lateral joins on it) and is the column the filter can push down. An
-- event whose device was deleted (`device_id` set null by the daemon's foreign
-- key) belongs to no device page, as before.
CREATE OR REPLACE VIEW ng_device_events AS
SELECT e.id, d.id AS device_id, e.event_type, e."timestamp", e.details, e.notified,
       e.timestamp_epoch
FROM ng_devices d
CROSS JOIN LATERAL (
    SELECT x.id, x.event_type, x."timestamp", x.details, x.notified, x.timestamp_epoch
    FROM ng_events x
    WHERE x.device_id = d.id
    ORDER BY x."timestamp" DESC
    LIMIT 10000
) e;

-- ---------------------------------------------------------------------------
-- Security events: one definition for the page and the overview
-- ---------------------------------------------------------------------------
-- The six types are the daemon's `recent_security_events` set, and exactly the
-- predicate of its partial ng_events_security_idx, so the planner can answer a
-- count of this view from that index alone. They must stay in step with
-- netgrasp_core::model::SECURITY_EVENT_TYPES; a test asserts they do.
--
-- /events/security reads this view (021), and so does the overview below, so
-- the total on the page and the number on the overview are one statement over
-- one relation, `SELECT count(*) FROM ng_security_events`, rather than two
-- statements that had to be kept equal by hand. The rows read the time index
-- newest first and keep the security events among them, which are about one
-- event in 25.
CREATE OR REPLACE VIEW ng_security_events AS
SELECT e.id, e.device_id, e.event_type, e."timestamp", e.details, e.notified, e.timestamp_epoch
FROM ng_events e
WHERE e.event_type IN ('arp_scan', 'arp_spoof', 'gratuitous_arp',
                       'identity_change', 'ip_conflict', 'rogue_dhcp');

-- ---------------------------------------------------------------------------
-- The overview, counting security events through that view
-- ---------------------------------------------------------------------------
-- 007's definition with its two security counts read from ng_security_events.
-- Same columns in the same order, which `CREATE OR REPLACE VIEW` requires and
-- ng_home (013) relies on; the other columns are 007's, word for word.
CREATE OR REPLACE VIEW ng_overview AS
SELECT
    1::bigint                                     AS id,
    to_char(now(), 'YYYY-MM-DD')                  AS today,
    'home'::text                                  AS home_state,
    'this_week'::text                             AS new_period,
    (SELECT count(*) FROM ng_people WHERE state = 'home') AS people_home,
    (SELECT count(*) FROM ng_people)              AS people_total,
    (SELECT count(*) FROM ng_devices
      WHERE state = 'online' AND NOT hidden)      AS devices_online,
    (SELECT count(*) FROM ng_devices_new
      WHERE NOT hidden)                           AS devices_new,
    (SELECT count(*) FROM ng_person_movements
      WHERE day = to_char(now(), 'YYYY-MM-DD'))   AS movements_today,
    (SELECT count(*) FROM ng_security_events)     AS security_events,
    (SELECT count(*) FROM ng_security_events
      WHERE "timestamp" > now() - INTERVAL '24 hours')
                                                  AS security_events_24h,
    EXTRACT(EPOCH FROM (now() AT TIME ZONE 'UTC'))::bigint AS generated_at_epoch;
