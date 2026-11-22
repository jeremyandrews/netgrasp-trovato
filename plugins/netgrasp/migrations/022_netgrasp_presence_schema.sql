-- netgraspd V7's two presence settings, guarded the way 015 copies V4's
-- tables. Forward-only; no rollback. Safe to run twice.
--
--   ng_devices.counts_for_presence  BOOLEAN, nullable. NULL decides by device
--                                   type (phone and wearable count), TRUE or
--                                   FALSE is a person's choice for the device.
--   ng_people.track_presence        BOOLEAN NOT NULL DEFAULT FALSE. Whether
--                                   anybody keeps track of whether this person
--                                   is home.
--
-- Both are user owned: this plugin writes them, from the ng_device field
-- "Counts for presence" and the ng_person field "Track whether they are home",
-- exactly as it writes notify and notify_arrive. The daemon only reads them.
--
-- Each column is added only where it is missing and only where this role owns
-- the table, the rule 017 follows: on a shared install the daemon has migrated
-- first (V7 adds both, IF NOT EXISTS, so either order ends in one schema), and
-- an ALTER TABLE on a table another role owns fails `trovato plugin install`
-- even when it would have done nothing.
--
-- An install with no daemon at all (the demo; no refinery_schema_history) also
-- gets the columns netgraspd V5 added to the tables 001 copies, and V7's
-- `untracked` default for ng_people.state, so the plugin's copy has the
-- daemon's V7 columns and `the_plugin_migration_is_a_faithful_copy_of_the_daemons_schema`
-- holds. V6's `notify DEFAULT FALSE` is not copied: it is not a presence
-- change, and a standalone install keeps the default it was created with.
-- Where a daemon exists those are its to add: its V5 adds them without
-- IF NOT EXISTS, and adding them here first would stop it migrating.
DO $$
DECLARE
    standalone boolean := to_regclass('refinery_schema_history') IS NULL;
    col        text[];
    tbl        regclass;
BEGIN
    FOREACH col SLICE 1 IN ARRAY ARRAY[
        -- table, column, definition, standalone only
        ARRAY['ng_people', 'last_arrived_at_epoch',
              'BIGINT GENERATED ALWAYS AS (EXTRACT(EPOCH FROM (last_arrived_at AT TIME ZONE ''UTC''))::bigint) STORED',
              'true'],
        ARRAY['ng_people', 'last_departed_at_epoch',
              'BIGINT GENERATED ALWAYS AS (EXTRACT(EPOCH FROM (last_departed_at AT TIME ZONE ''UTC''))::bigint) STORED',
              'true'],
        ARRAY['ng_events', 'person_item_id', 'UUID', 'true'],
        ARRAY['ng_devices', 'counts_for_presence', 'BOOLEAN', 'false'],
        ARRAY['ng_people', 'track_presence', 'BOOLEAN NOT NULL DEFAULT FALSE', 'false']
    ] LOOP
        CONTINUE WHEN col[4] = 'true' AND NOT standalone;
        tbl := to_regclass(col[1]);
        CONTINUE WHEN tbl IS NULL;
        CONTINUE WHEN EXISTS (
            SELECT 1 FROM pg_catalog.pg_attribute
            WHERE attrelid = tbl AND attname = col[2] AND NOT attisdropped
        );
        IF NOT pg_catalog.pg_has_role(
            current_user,
            (SELECT relowner FROM pg_catalog.pg_class WHERE oid = tbl),
            'USAGE'
        ) THEN
            RAISE NOTICE 'netgrasp: % has no column % and this role does not own it; upgrade netgraspd to V7 to add it',
                col[1], col[2];
            CONTINUE;
        END IF;
        EXECUTE format('ALTER TABLE %s ADD COLUMN %I %s', tbl, col[2], col[3]);
    END LOOP;

    IF standalone THEN
        -- V7's default, on a table this role created: nobody is tracked until
        -- somebody switches them on.
        IF to_regclass('ng_people') IS NOT NULL AND pg_catalog.pg_has_role(
            current_user,
            (SELECT relowner FROM pg_catalog.pg_class WHERE oid = 'ng_people'::regclass),
            'USAGE'
        ) THEN
            ALTER TABLE ng_people ALTER COLUMN state SET DEFAULT 'untracked';
        END IF;
    END IF;
END $$;
