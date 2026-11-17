-- ---------------------------------------------------------------------------
-- People: a merge keeps the history of the person merged away. Forward-only;
-- no rollback. Safe to run twice.
--
-- docs/UI.md, "People model" and U2 check 6.

-- ---------------------------------------------------------------------------
-- The record of who was merged into whom
-- ---------------------------------------------------------------------------
-- A person's comings and goings are the daemon's `person_arrived` and
-- `person_departed` events, and each one carries the person's Item id and the
-- name they had in `details`. `ng_events` is the daemon's table: the plugin does
-- not rewrite its rows, so when Jamie (2) is merged into Jamie, Jamie (2)'s old
-- arrivals still name Jamie (2)'s id.
--
-- This table is what attributes them. One row per person merged away: their id,
-- the id of the person they were merged into, and the name they had. The
-- plugin owns it; the daemon never reads it.
--
-- It stays one hop deep. When Jamie is later merged into Jem, the rows that
-- pointed at Jamie are re-pointed at Jem in the same statement that records the
-- second merge (netgrasp_core::queries::MERGE_PERSON_RECORD), so the view below
-- needs one join however many merges came before.
CREATE TABLE IF NOT EXISTS ng_person_merge (
    from_item_id UUID        PRIMARY KEY,
    into_item_id UUID        NOT NULL,
    from_name    TEXT        NOT NULL,
    merged_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS ng_person_merge_into ON ng_person_merge (into_item_id);

-- ---------------------------------------------------------------------------
-- Comings and goings, attributed through a merge
-- ---------------------------------------------------------------------------
-- 007's view, with one change: an event of a person who was merged away is
-- attributed to the person they were merged into. `person_item_id` is the kept
-- person's id, so their page lists both histories, and `person_name` is the
-- kept person's current name.
--
-- Every column is the one 007 declares, in the same order and of the same type,
-- which is what `CREATE OR REPLACE VIEW` requires and what the views built on
-- this one (ng_overview, ng_activity) depend on.
--
-- A person who was deleted keeps the name the event was recorded under, as in
-- 007. A person who was merged into somebody since deleted does too: neither
-- join finds a mirror row, and `details` still has the name.
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
WHERE e.event_type IN ('person_arrived', 'person_departed');
