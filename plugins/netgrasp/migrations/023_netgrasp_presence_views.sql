-- Presence is opt in (netgraspd V7): every presence surface says "Not tracked"
-- for a person nobody is tracking, and Home and People say so when tracking is
-- off for the whole install. Forward-only; no rollback. Safe to run twice.
--
-- netgraspd V7 gave ng_people.state a third value, `untracked`: the install's
-- presence_tracking is off, the person's own track_presence is off, or they
-- own no device that counts (a phone or a wearable, unless somebody chose
-- otherwise for a device). An untracked person is never home and never away.
-- Every count here that said "home" already read `state = 'home'`, so an
-- untracked person was never counted home; this adds the third state where a
-- page lists people, so nobody untracked is listed as away either.
--
-- Two new user-owned columns are read here, ng_people.track_presence and
-- ng_devices.counts_for_presence (022). They are read through to_jsonb(row)
-- rather than by name, so these views still build on an install where 022
-- could not add them (a role that does not own the daemon's tables, in front
-- of a daemon not yet at V7): the value reads NULL there, and the page says
-- "Not tracked", which is what that daemon's absence of the setting means.

-- ---------------------------------------------------------------------------
-- The install's switch
-- ---------------------------------------------------------------------------
-- The daemon publishes its `presence_tracking` setting in
-- ng_daemon_status.presence_tracking (its V7; the table is V5). TRUE or FALSE as
-- published, NULL when there is no daemon status to read: no daemon at all (the
-- demo), or a daemon older than V7. Home and People print their "tracking is
-- off" line only for FALSE, a daemon that has said so.
--
-- A function rather than a subquery in each view, because a view cannot name a
-- table that may not exist, and the plugin cannot create ng_daemon_status
-- itself: the daemon's V5 creates it unguarded.
CREATE OR REPLACE FUNCTION ng_presence_tracking() RETURNS boolean
LANGUAGE plpgsql STABLE AS $$
DECLARE
    published boolean;
BEGIN
    IF to_regclass('ng_daemon_status') IS NULL THEN
        RETURN NULL;
    END IF;
    EXECUTE 'SELECT (to_jsonb(s) ->> ''presence_tracking'')::boolean
               FROM ng_daemon_status s ORDER BY s.id LIMIT 1'
       INTO published;
    RETURN published;
END
$$;

-- ---------------------------------------------------------------------------
-- Three new views, not three widened ones
-- ---------------------------------------------------------------------------
-- Each record type below gains a column or three, and each is declared over a
-- NEW view on top of the one it read, which the manifest now names instead.
-- `CREATE OR REPLACE VIEW` may add a column but never drop one, so widening
-- 006's, 007's or 013's view in place would make that migration fail when it
-- is run again, as the host-in-the-loop suite does before every test. 012 and
-- 013 made the same choice for the same reason. A view over each leaves every
-- earlier migration exactly as re-runnable as it was.

-- People, with their presence settings. 007's ng_people_presence plus:
--
--   track_presence     the person's own switch;
--   presence_tracking  the install's, from ng_presence_tracking();
--   state_rank         0 home, 1 away, 2 anything else (untracked), so a page
--                      that lists everybody can list them in that order.
CREATE OR REPLACE VIEW ng_people_presence_status AS
SELECT
    pp.*,
    COALESCE((to_jsonb(p) ->> 'track_presence')::boolean, FALSE) AS track_presence,
    ng_presence_tracking()                                       AS presence_tracking,
    CASE pp.state WHEN 'home' THEN 0 WHEN 'away' THEN 1 ELSE 2 END AS state_rank
FROM ng_people_presence pp
JOIN ng_people p ON p.item_id = pp.item_id;

-- Home's row. 013's ng_home plus:
--
--   untracked_state    'untracked', the join key for the people nobody is
--                      tracking, the way away_state is for the people who are
--                      out;
--   presence_tracking  the install's switch, for the "tracking is off" line;
--   people_away        counted as people_home is, so neither counts anybody
--                      untracked;
--   people_untracked   everybody who is neither.
CREATE OR REPLACE VIEW ng_home_presence AS
SELECT
    h.*,
    'untracked'::text AS untracked_state,
    ng_presence_tracking() AS presence_tracking,
    (SELECT count(*) FROM ng_people WHERE state = 'away') AS people_away,
    (SELECT count(*) FROM ng_people WHERE state NOT IN ('home', 'away'))
                                                  AS people_untracked
FROM ng_home h;

-- Devices, with their owner's presence state. 012's ng_devices_listed plus
-- `owner_state`, which Who is home filters on below.
CREATE OR REPLACE VIEW ng_devices_listed_presence AS
SELECT
    l.*,
    p.state AS owner_state
FROM ng_devices_listed l
LEFT JOIN ng_people p ON p.item_id = l.owner_item_id;

-- ---------------------------------------------------------------------------
-- Home lists the people nobody is tracking
-- ---------------------------------------------------------------------------
-- A third include beside `home` and `away`, joined the same way, on
-- `untracked_state`. One muted line on the page, like Away.
UPDATE gather_query SET
    definition = jsonb_set(definition, '{includes,untracked}', '{
        "parent_field": "untracked_state",
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
    }'::jsonb),
    changed = EXTRACT(EPOCH FROM NOW())::bigint
WHERE query_id = 'ng_overview';

-- ---------------------------------------------------------------------------
-- People: Home now lists everybody, in three groups
-- ---------------------------------------------------------------------------
-- /people/home listed only `state = 'home'`. It now lists every person, home
-- first, then away, then not tracked, and the template draws the three groups
-- and the "tracking is off" line. Dropping the filter keeps it in step with the
-- overview's `home` include, which has none of its own (its join supplies the
-- state); the host-in-the-loop suite holds the two together on filters.
UPDATE gather_query SET
    description = 'Who is home, who is away, and who is not tracked',
    definition = jsonb_set(jsonb_set(definition, '{filters}', '[]'::jsonb), '{sorts}', '[
        {"field": "state_rank", "direction": "asc", "nulls": null},
        {"field": "last_arrived", "direction": "asc", "nulls": null},
        {"field": "name", "direction": "asc", "nulls": null}
    ]'::jsonb),
    display = jsonb_set(display, '{empty_text}', '"Nobody has been added yet."'::jsonb),
    changed = EXTRACT(EPOCH FROM NOW())::bigint
WHERE query_id = 'ng_people_home';

-- Home's own `home` include and /people/home are one list in two places, and
-- the suite holds their filters and sorts equal. The include keeps its join on
-- `state = 'home'`, so the leading state_rank sorts nothing there; it is there
-- so the two definitions stay the same definition.
UPDATE gather_query SET
    definition = jsonb_set(definition, '{includes,home,definition,sorts}', '[
        {"field": "state_rank", "direction": "asc", "nulls": null},
        {"field": "last_arrived", "direction": "asc", "nulls": null},
        {"field": "name", "direction": "asc", "nulls": null}
    ]'::jsonb),
    changed = EXTRACT(EPOCH FROM NOW())::bigint
WHERE query_id = 'ng_overview';

-- ---------------------------------------------------------------------------
-- People: each card carries the daemon's presence row
-- ---------------------------------------------------------------------------
-- /people is a gather over `item`, so it knew a person's own switch
-- (`field_track_presence`) and nothing the daemon decided. A singular include
-- joins each person's ng_people_presence row on the Item id, which gives the
-- card Home, Away or Not tracked and gives the page the install's switch for
-- its "tracking is off" line.
UPDATE gather_query SET
    definition = jsonb_set(definition, '{includes,presence}', '{
        "parent_field": "id",
        "child_field": "item_id",
        "singular": true,
        "definition": {
            "record_type": "ng_person_presence",
            "fields": [],
            "filters": [],
            "sorts": [],
            "relationships": [],
            "includes": {}
        },
        "display": {"items_per_page": 1000}
    }'::jsonb),
    changed = EXTRACT(EPOCH FROM NOW())::bigint
WHERE query_id = 'ng_person_list';

-- ---------------------------------------------------------------------------
-- Who is home, device-shaped: only the people the daemon counts as home
-- ---------------------------------------------------------------------------
-- /gather/ng_who_is_home grouped every online owned device by owner and called
-- each owner home. With presence opt in that is wrong twice: an untracked
-- person's laptop being online says nothing, and neither does a tracked
-- person's laptop when their phone has left. The page now lists only owners
-- whose ng_people.state is `home`, which is the daemon's own answer.
--
-- The filter is on `owner_state`, which the record type reads from
-- ng_devices_listed_presence above.
UPDATE gather_query SET
    definition = jsonb_set(definition, '{filters}',
        COALESCE(
            (SELECT jsonb_agg(f) FROM jsonb_array_elements(definition -> 'filters') f
              WHERE f ->> 'field' <> 'owner_state'),
            '[]'::jsonb
        ) || '[{"field": "owner_state", "operator": "equals", "value": "home",
                "exposed": false, "exposed_label": null}]'::jsonb),
    changed = EXTRACT(EPOCH FROM NOW())::bigint
WHERE query_id = 'ng_who_is_home';
