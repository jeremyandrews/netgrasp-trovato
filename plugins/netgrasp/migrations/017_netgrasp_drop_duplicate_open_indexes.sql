-- Drop the plugin's copies of the daemon's two "one open row per device"
-- indexes, where the daemon already has its own. Forward-only; no rollback.
--
-- 001 creates `uniq_ng_presence_open` and `uniq_ng_location_open` whenever the
-- kernel's role owns the table, which on a shared install, migrated by the
-- daemon first as documented, is also true when the kernel connects as the
-- daemon's own role. The daemon has already created the same unique partial
-- index under its own name (`ng_presence_open_idx`, `ng_location_history_open_idx`),
-- so every presence and location write maintained two identical indexes, and
-- the daemon's preflight does not look for extras, so they outlived upgrades.
--
-- What is dropped is only ever one of the two names 001 gives, and only when
-- some OTHER valid unique index on the same table covers exactly `device_id`
-- with exactly the same predicate. A daemon index is never dropped. Where the
-- plugin created the table itself (an install with no daemon), its index is the
-- only one and stays, because it is what keeps one open row per device. Where
-- the kernel's role does not own the table, 001 created nothing and there is
-- nothing this role could drop, so the block does nothing.
--
-- 001 is not edited: it has already run on every existing install, and the
-- kernel runs each file once.
DO $$
DECLARE
    pair   text[];
    tbl    regclass;
    ours   oid;
BEGIN
    FOREACH pair SLICE 1 IN ARRAY ARRAY[
        ARRAY['ng_presence', 'uniq_ng_presence_open'],
        ARRAY['ng_location_history', 'uniq_ng_location_open']
    ] LOOP
        tbl := to_regclass(pair[1]);
        CONTINUE WHEN tbl IS NULL;

        SELECT i.indexrelid INTO ours
        FROM pg_catalog.pg_index i
        JOIN pg_catalog.pg_class c ON c.oid = i.indexrelid
        WHERE i.indrelid = tbl AND c.relname = pair[2];
        CONTINUE WHEN ours IS NULL;

        CONTINUE WHEN NOT pg_catalog.pg_has_role(
            current_user,
            (SELECT relowner FROM pg_catalog.pg_class WHERE oid = tbl),
            'USAGE'
        );

        IF EXISTS (
            SELECT 1
            FROM pg_catalog.pg_index other, pg_catalog.pg_index mine
            WHERE mine.indexrelid = ours
              AND other.indrelid = tbl
              AND other.indexrelid <> ours
              AND other.indisunique
              AND other.indisvalid
              AND other.indkey::text = mine.indkey::text
              AND other.indexprs IS NULL
              AND pg_catalog.pg_get_expr(other.indpred, other.indrelid)
                  IS NOT DISTINCT FROM pg_catalog.pg_get_expr(mine.indpred, mine.indrelid)
        ) THEN
            EXECUTE format('DROP INDEX %s', ours::regclass);
            RAISE NOTICE 'netgrasp: dropped %, a duplicate of the daemon''s own index on %',
                pair[2], pair[1];
        END IF;
    END LOOP;
END $$;
