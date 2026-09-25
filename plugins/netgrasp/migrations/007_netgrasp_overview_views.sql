-- The views behind the overview page. Forward-only; no rollback.
--
-- Four views, all plugin-owned, none of them a change to a daemon table. Each is
-- declared as a read-only record type in netgrasp.info.toml, so a gather can read
-- it, and the assistant's read tools select from the same four, so a page and a
-- conversation asked the same question get the same rows.
--
-- This file is schema only, like 001: nothing below writes into a kernel table,
-- so CI applies it on top of the daemon's DDL on its own and the schema test runs
-- the assistant's statements against it. The gathers and routes over these views
-- are 008.
--
-- ===========================================================================
-- Why views, and not gather filters
-- ===========================================================================
-- Three of the four questions are relative to the clock: who arrived TODAY,
-- which devices are new THIS WEEK, how many security events in the last day. A
-- gather filter cannot say any of that. `ContextualValue` offers `current_time`
-- and `current_date` and no offset, so "seven days before now" is not a value a
-- filter can hold; and `current_date` is resolved in the kernel process's local
-- time while the rows' timestamps are compared in the database's, which can be
-- two different days. A view evaluates `now()` in the same session that compares
-- the rows, so "today" means one thing on the whole page.
--
-- The fourth is a count, and a gather cannot count at all
-- (G-NO-GATHER-AGGREGATION), which is why the overview is a one-row view whose
-- columns ARE the counts rather than a page of rows whose length is one.
--
-- "Today" is the database session's calendar day, in its TimeZone setting. On
-- the demo and on a default Postgres that is UTC. A household in Rome whose
-- database says UTC sees "today" roll over at 01:00 or 02:00 local time; setting
-- the database's TimeZone is the fix, and nothing here needs to change for it.
--
-- ===========================================================================
-- Columns are listed, and times are epoch twins
-- ===========================================================================
-- Same two rules as 006. A `*` would freeze today's column list into the view.
-- And a `timestamptz` is `null` through the plugin's `db` host, which the
-- assistant reads these views through, so every time below is a BIGINT. On
-- ng_devices and ng_events that is the daemon's own generated twin. ng_people
-- has no twins (the daemon's V3 gave every other table one and skipped it), so
-- ng_people_presence computes them here, with the same expression the daemon's
-- generated columns use.

-- ---------------------------------------------------------------------------
-- People, with the arrival times the plugin can actually read
-- ---------------------------------------------------------------------------
-- `state` is the daemon's word for it: home when any device the person owns is
-- online (netgraspd, src/people/mod.rs). It is not recomputed here from the
-- device rows, because `last_arrived_at` is the daemon's too and "home since"
-- has to be the arrival that made them home, not an estimate of it.
--
-- `devices_online` is counted for the page's second line ("2 devices online"),
-- and counts what /who-is-home counts: online, not hidden.
CREATE OR REPLACE VIEW ng_people_presence AS
SELECT
    p.item_id,
    p.name,
    p.state,
    p.current_location,
    EXTRACT(EPOCH FROM (p.last_arrived_at AT TIME ZONE 'UTC'))::bigint  AS last_arrived_at_epoch,
    EXTRACT(EPOCH FROM (p.last_departed_at AT TIME ZONE 'UTC'))::bigint AS last_departed_at_epoch,
    (SELECT count(*)
       FROM ng_devices d
      WHERE d.owner_item_id = p.item_id
        AND d.state = 'online'
        AND NOT d.hidden) AS devices_online
FROM ng_people p;

-- ---------------------------------------------------------------------------
-- Arrivals and departures
-- ---------------------------------------------------------------------------
-- The daemon's two person events, `person_arrived` and `person_departed`
-- (EventType::as_str), with what their `details` carry lifted into columns: the
-- person, where they arrived, and the edge access point that is the evidence
-- for it ("via"). A departure has no location, only a via.
--
-- The name is the mirror's CURRENT name when the person still exists, and the
-- name the event was recorded under when they do not. An event outlives a
-- deleted person, and "Arlo left at 08:10" is still true after Arlo's Item is
-- gone.
--
-- The join is on text because `details` is JSON: `details ->> 'person_item_id'`
-- is a string, and casting it to uuid would raise on the first malformed row
-- rather than simply not matching it.
--
-- `day` is the event's calendar day in the session's time zone, as text, which
-- is what the overview's include matches against its own `today`.
CREATE OR REPLACE VIEW ng_person_movements AS
SELECT
    e.id,
    e.event_type,
    e.timestamp_epoch,
    to_char(e."timestamp", 'YYYY-MM-DD')             AS day,
    e.details ->> 'person_item_id'                   AS person_item_id,
    COALESCE(p.name, e.details ->> 'person')         AS person_name,
    e.details ->> 'location'                         AS location,
    e.details ->> 'via'                              AS via,
    e.device_id,
    d.mac                                            AS device_mac,
    d.display_name                                   AS device_display_name,
    d.resolved_name                                  AS device_resolved_name,
    d.hostname                                       AS device_hostname,
    d.trovato_item_id                                AS device_item_id
FROM ng_events e
LEFT JOIN ng_people  p ON p.item_id::text = e.details ->> 'person_item_id'
LEFT JOIN ng_devices d ON d.id = e.device_id
WHERE e.event_type IN ('person_arrived', 'person_departed');

-- ---------------------------------------------------------------------------
-- Devices first seen in the last seven days
-- ---------------------------------------------------------------------------
-- A new device is the one thing on the network somebody should look at: name
-- it, give it an owner, or find out what it is. The columns are the ones that
-- question needs. `device_type` and `device_type_confidence` are the daemon's
-- fingerprint verdict and how sure it is; `identity_source` and
-- `identity_confidence` say where its name came from.
--
-- `period` is constant, and it is here for the overview's include to match on:
-- an include joins a child to its parent by equal values, and "first seen in
-- the last week" is a range, not a value. The view is already the range, so its
-- rows all say which one, and the overview's row says the same word.
--
-- Hidden devices are in the view and filtered out by the gathers, the same way
-- every other device listing does it, so the view stays one definition of "new".
CREATE OR REPLACE VIEW ng_devices_new AS
SELECT
    d.id,
    d.mac,
    d.display_name,
    d.resolved_name,
    d.hostname,
    d.mdns_name,
    d.vendor,
    d.device_type,
    d.device_type_confidence,
    d.os_family,
    d.identity_source,
    d.identity_confidence,
    d.state,
    d.last_ip,
    d.hidden,
    d.notify,
    d.owner_item_id,
    p.name AS owner_name,
    d.trovato_item_id,
    d.first_seen_at_epoch,
    d.last_seen_at_epoch,
    'this_week'::text AS period
FROM ng_devices d
LEFT JOIN ng_people p ON p.item_id = d.owner_item_id
WHERE d.first_seen_at > now() - INTERVAL '7 days';

-- ---------------------------------------------------------------------------
-- The overview: one row, whose columns are the counts
-- ---------------------------------------------------------------------------
-- The page's parent row. Its three text columns are what its includes join on
-- (`home_state` to a person's state, `today` to a movement's day, `new_period`
-- to a new device's period); the rest are the figures the page prints.
--
-- `security_events` counts exactly what /events/security lists, with the same
-- six types, so the number on the overview and the pager total on the page it
-- links to are the same number. The list must stay in step with
-- netgrasp_core::model::SECURITY_EVENT_TYPES and with the gather in 002; a unit
-- test asserts all three agree. `security_events_24h` is the same count over the
-- last day, which is the number that says whether anything is happening now.
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
    (SELECT count(*) FROM ng_events
      WHERE event_type IN ('arp_scan', 'arp_spoof', 'gratuitous_arp',
                           'identity_change', 'ip_conflict', 'rogue_dhcp'))
                                                  AS security_events,
    (SELECT count(*) FROM ng_events
      WHERE event_type IN ('arp_scan', 'arp_spoof', 'gratuitous_arp',
                           'identity_change', 'ip_conflict', 'rogue_dhcp')
        AND "timestamp" > now() - INTERVAL '24 hours')
                                                  AS security_events_24h,
    EXTRACT(EPOCH FROM (now() AT TIME ZONE 'UTC'))::bigint AS generated_at_epoch;
