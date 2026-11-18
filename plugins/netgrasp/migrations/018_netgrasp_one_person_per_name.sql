-- One person per name, enforced by the database. Forward-only; no rollback.
-- Safe to run twice.
--
-- "jamie " is Jamie: netgrasp_core::assist::same_name compares names trimmed
-- and lower-cased, and find_or_create_person refuses to create a second person
-- whose name is the same by that rule. That check runs in the plugin, so two
-- creates of "Jamie" that arrive together can both pass it before either has
-- written its mirror row, and both people get made. This index is what makes
-- the second mirror write fail instead; find_or_create_person then takes back
-- the Item it just created and returns the person who won.
--
-- The key is `lower(btrim(name))`, the SQL spelling of `name_key`:
--
-- - trimming: the mirror upsert (build_person_upsert) stores the name already
--   trimmed by Rust's `str::trim`, so every row the plugin writes has no
--   surrounding whitespace of any kind, and `btrim` (which strips spaces) only
--   matters for a row written some other way, such as the daemon's `[[people]]`;
-- - lower-casing: Postgres's `lower` follows the database's ctype. Under a
--   UTF-8 ctype (en_US.utf8, the default of the postgres images) it lower-cases
--   "ÉLODIE" to "élodie" as Rust does. It differs from Rust's `to_lowercase` on
--   a few characters (Turkish "İ", Greek final sigma), and under a C ctype it
--   lower-cases ASCII only. Where the two disagree the database decides: the
--   conflict branch looks the existing person up by this same expression.
--
-- ng_people is the daemon's table, so this is guarded on ownership as 015's
-- index is: a kernel connecting as a role that does not own it cannot create an
-- index on it, and must not fail `trovato plugin install` trying. On such an
-- install the table's owner creates the index (README, "Which database role").
--
-- If two people already share a name by this rule the index cannot be built.
-- Nothing is deleted or renamed to make it fit: the migration fails and names
-- them, and somebody decides which one stays.
DO $$
DECLARE
    clashes text;
BEGIN
    IF NOT pg_catalog.pg_has_role(
        current_user,
        (SELECT relowner FROM pg_catalog.pg_class WHERE oid = 'ng_people'::regclass),
        'USAGE'
    ) THEN
        RAISE NOTICE 'netgrasp: this role does not own ng_people, so the one-person-per-name index is left to its owner';
        RETURN;
    END IF;

    SELECT string_agg(names, '; ' ORDER BY names) INTO clashes
    FROM (
        SELECT string_agg(format('%L (%s)', name, item_id), ', ' ORDER BY name, item_id) AS names
        FROM ng_people
        GROUP BY lower(btrim(name))
        HAVING count(*) > 1
    ) AS dup;

    IF clashes IS NOT NULL THEN
        RAISE EXCEPTION 'netgrasp: two or more people share a name, ignoring case and surrounding spaces: %', clashes
            USING HINT = 'Merge or rename all but one of each, then run this migration again. Nothing was changed.';
    END IF;

    CREATE UNIQUE INDEX IF NOT EXISTS ng_people_name_key ON ng_people (lower(btrim(name)));
END $$;
