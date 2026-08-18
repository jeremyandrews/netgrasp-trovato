-- A device row that carries its owner's name.
-- Forward-only; no rollback.
--
-- ===========================================================================
-- The problem
-- ===========================================================================
-- Every device page labelled an owner with a uuid chip — `000000000001` where a
-- person's name belongs. Not a cosmetic shortcut: a gather reads ONE record type,
-- so the device pages had no `ng_people` row to take a name from, and a wall
-- display showing hex instead of "Jamie" is the page failing at its one job.
--
-- ===========================================================================
-- Why a view, and why not the alternatives
-- ===========================================================================
-- Four ways to get a name next to a device, three of them wrong:
--
--   * A gather `relationships` join. Tried against a live gather: inert. The
--     kernel rewrites a record gather's `base_table` to the record type's table
--     and the row shape comes from `row_to_json` over that, so a joined column
--     never reaches the template. Making it reach would be a change to Trovato's
--     gather engine.
--   * A denormalized `owner_name` column on ng_devices. That table is the
--     daemon's, and its columns are three disjoint sets — daemon-owned,
--     user-owned, link-owned — which is what makes "the two writers never
--     collide" checkable (netgrasp_core::columns). A fourth kind of column,
--     written by neither, breaks the property and the schema-faithfulness test
--     with it.
--   * Resolving the name in the template. A template cannot query.
--
--   * A VIEW, declared as the record type's backing table. The kernel checks
--     only that a record type's table is in the plugin's own allowlist, so a
--     view is admissible, and `SELECT row_to_json(t) FROM (SELECT * FROM v) t`
--     does not care that `v` is not a table. The join happens once, in SQL,
--     where a join belongs, and nothing about the daemon's schema changes.
--
-- ===========================================================================
-- Columns are listed, not `d.*`
-- ===========================================================================
-- Postgres expands `*` at CREATE VIEW time, so `SELECT d.*` would freeze
-- today's column list into the view and silently omit any column the daemon adds
-- later — a missing column with no error anywhere. Listing them makes the view
-- exactly the record type's projection plus the owner's name, and
-- `the_owner_view_carries_every_column_the_record_type_maps` in src/lib.rs
-- asserts the two lists agree. Add a field to the record type without adding the
-- column here and that test fails, instead of a page rendering a blank cell.
--
-- LEFT JOIN, not JOIN: most devices have no owner, and an inner join would
-- delete them from every device page.
CREATE OR REPLACE VIEW ng_devices_with_owner AS
SELECT
    d.id,
    d.mac,
    d.display_name,
    d.hidden,
    d.notify,
    d.owner_item_id,
    d.resolved_name,
    d.hostname,
    d.mdns_name,
    d.vendor,
    d.device_type,
    d.os_family,
    d.state,
    d.last_ip,
    d.last_ipv6,
    d.current_ap,
    d.current_location,
    d.first_seen_at_epoch,
    d.last_seen_at_epoch,
    d.trovato_item_id,
    -- The whole point of the view. Null for an unowned device, and null for an
    -- owner id with no mirror row — which is a real state, because ng_people is
    -- written by the plugin's person taps and a device can name an id whose Item
    -- has since been deleted. The templates render the id chip in that case
    -- rather than an empty cell, so the id stays visible when it is all there is.
    p.name AS owner_name
FROM ng_devices d
LEFT JOIN ng_people p ON p.item_id = d.owner_item_id;

-- ---------------------------------------------------------------------------
-- Who is home reads people, sorted by name
-- ---------------------------------------------------------------------------
-- The page groups online devices by owner. With a name on the row it can sort by
-- the name rather than by the uuid, so the cards come out in an order a reader
-- recognises instead of in UUIDv7 creation order. Grouping still keys on
-- owner_item_id, because two people may share a name and must stay two cards.
--
-- The sort is rewritten wholesale rather than appended to: `owner_id` was the
-- first key precisely to make a person's devices arrive adjacent, and
--`owner_name` does that job better only if it comes first.
UPDATE gather_query SET
    definition = jsonb_set(definition, '{sorts}', '[
        {"field": "owner_name", "direction": "asc",  "nulls": null},
        {"field": "owner_id",   "direction": "asc",  "nulls": null},
        {"field": "last_seen",  "direction": "desc", "nulls": null}
    ]'::jsonb),
    changed = EXTRACT(EPOCH FROM NOW())::bigint
WHERE query_id = 'ng_who_is_home';
