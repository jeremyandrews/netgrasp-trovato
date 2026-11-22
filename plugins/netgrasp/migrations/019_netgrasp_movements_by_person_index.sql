-- Read arrivals and departures through the daemon's person index, where the
-- daemon has one. Forward-only; no rollback.
--
-- `ng_person_movements` (007, redefined in 014) selects
-- `event_type IN ('person_arrived', 'person_departed')` from `ng_events`, and
-- nothing the daemon indexes serves that filter, nor the `day` the overview and
-- /people/movements filter on: `day` is `to_char("timestamp", …)`, which no
-- index can answer. Every read of the view was therefore a sequential scan of
-- the whole event log, which on a site that keeps 90 days is millions of rows of
-- `ip_changed`. The overview reads the view twice (its `movements_today` count
-- and its movements include), so the front page, and the status check that
-- polls it every minute, spent about 280 ms of its time in two such scans.
--
-- netgraspd's V5 added `ng_events.person_item_id`, set on every person event and
-- null on every other, with a partial index `ng_events_person_idx` over the rows
-- where it is not null. Adding `e.person_item_id IS NOT NULL` to the view's
-- filter lets the planner read that index instead of the table: the scan
-- touches the person events and nothing else, whatever the size of the log.
--
-- The view's columns, their order and their meaning are unchanged, which is what
-- `CREATE OR REPLACE VIEW` requires and what `ng_overview` (built on it) relies
-- on. `person_item_id` is still read from `details`, through the merge record,
-- exactly as 014 reads it.
--
-- The one row the new filter leaves out is a person event whose
-- `person_item_id` the daemon could not set: V5 backfills the column from
-- `details` only where that is a well-formed uuid, and every person event the
-- daemon writes since carries one. Such a row named nobody the mirror could
-- join to, so it could only ever have shown a bare name.
--
-- Where `ng_events` has no `person_item_id` (a daemon older than V5, or an
-- install where the plugin created the table itself from its V4 copy in 001),
-- this does nothing and 014's definition stands. A daemon upgraded to V5 after
-- this has run is picked up by running this file again: it is idempotent.
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_catalog.pg_attribute
        WHERE attrelid = to_regclass('ng_events')
          AND attname = 'person_item_id'
          AND NOT attisdropped
    ) THEN
        RETURN;
    END IF;

    EXECUTE $view$
CREATE OR REPLACE VIEW ng_person_movements AS
SELECT
    e.id,
    e.event_type,
    e.timestamp_epoch,
    to_char(e."timestamp", 'YYYY-MM-DD')                               AS day,
    COALESCE(m.into_item_id::text, e.details ->> 'person_item_id')     AS person_item_id,
    COALESCE(p.name, e.details ->> 'person')                           AS person_name,
    e.details ->> 'location'                                           AS location,
    e.details ->> 'via'                                                AS via,
    e.device_id,
    d.mac                                                              AS device_mac,
    d.display_name                                                     AS device_display_name,
    d.resolved_name                                                    AS device_resolved_name,
    d.hostname                                                         AS device_hostname,
    d.trovato_item_id                                                  AS device_item_id
FROM ng_events e
LEFT JOIN ng_person_merge m ON m.from_item_id::text = e.details ->> 'person_item_id'
LEFT JOIN ng_people  p ON p.item_id::text = COALESCE(m.into_item_id::text, e.details ->> 'person_item_id')
LEFT JOIN ng_devices d ON d.id = e.device_id
WHERE e.person_item_id IS NOT NULL
  AND e.event_type IN ('person_arrived', 'person_departed')
$view$;
END
$$;
