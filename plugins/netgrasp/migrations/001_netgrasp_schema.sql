-- Netgrasp schema. Forward-only; no rollback.
--
-- THIS FILE IS A COPY. The native netgrasp daemon owns every `ng_` table below:
-- it creates them, it migrates them, and it is the only writer of the columns
-- marked daemon-owned. What is reproduced here is the daemon's landed schema
-- (milestones 1 through 3) with `IF NOT EXISTS` guards added and nothing else
-- changed — same columns, same types, same order.
--
-- On a shared install the daemon must migrate FIRST. This file cannot converge a
-- daemon-created table onto the shape below: `CREATE TABLE IF NOT EXISTS` on an
-- existing table is a silent no-op whatever its columns are, so a stale daemon
-- schema stays stale and this migration will not say so. (An earlier version of
-- this file claimed convergence and shipped a set of `ADD COLUMN IF NOT EXISTS`
-- statements to achieve it. It never did: the two schemas disagreed on the
-- primary key type, on every timestamp, and on four column names, none of which
-- an `ADD COLUMN` can fix.)
--
-- It exists anyway, for two reasons (DESIGN.md Decision 8):
--
--   1. A plugin's effective DB allowlist is (migration-owned ∪ db_tables), and a
--      record type is only admitted over a table inside it. Declaring the tables
--      here and in db_tables makes the allowlist independent of how a future
--      kernel parses CREATE statements.
--   2. An install with no daemon yet must still be able to enable the plugin and
--      show empty pages rather than error.
--
-- Two shapes below are load-bearing for the plugin and are worth naming:
--
--   * Device ids are `BIGINT GENERATED ALWAYS AS IDENTITY`, not uuids. Every
--     query this plugin binds a device id into casts `::bigint`.
--   * Every `timestamptz` carries a generated `<column>_epoch` twin. The `db`
--     host decodes a fixed list of Postgres types and falls through to a string
--     decode for the rest (crates/kernel/src/host/db.rs), and a `timestamptz`
--     cannot decode as a string — it arrives as `null`. The plugin therefore
--     never reads a `timestamptz` column; it reads the twin, which is a
--     `BIGINT`. The twins are `GENERATED ALWAYS … STORED`, so no writer on
--     either side can put a value in one.
--
-- Column ownership is the load-bearing part and is enforced in code, not here:
-- netgrasp_core::columns names the three disjoint sets, and the write-back
-- statement builder generates its SET list from one of them so it cannot name a
-- column from another.
--
-- Indexes are the daemon's business. Only what the plugin needs to install
-- cleanly on a database with no daemon is copied: primary keys, unique
-- constraints, and the partial unique index that allows at most one open row
-- per device on ng_presence and on ng_location_history.

-- ---------------------------------------------------------------------------
-- Devices
-- ---------------------------------------------------------------------------
-- Three writers, three column groups:
--   daemon-owned : mac, resolved_name, identity_*, hostname, mdns_name, vendor,
--                  device_type*, os_family, state, last_ip, last_ipv6,
--                  last_interface, first_seen_at, last_seen_at, baseline,
--                  current_ap, current_location, sync_state
--   user-owned   : display_name, notes, hidden, notify, owner_item_id
--   plugin-owned : trovato_item_id
CREATE TABLE IF NOT EXISTS ng_devices (
    id                     BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    mac                    TEXT        NOT NULL UNIQUE,
    -- user owned: the plugin writes these, the daemon never does
    display_name           TEXT,
    notes                  TEXT,
    hidden                 BOOLEAN     NOT NULL DEFAULT FALSE,
    notify                 BOOLEAN     NOT NULL DEFAULT TRUE,
    owner_item_id          UUID,
    -- daemon owned identity
    resolved_name          TEXT,
    identity_source        TEXT,
    identity_confidence    REAL,
    hostname               TEXT,
    mdns_name              TEXT,
    vendor                 TEXT,
    device_type            TEXT,
    device_type_confidence REAL,
    os_family              TEXT,
    -- daemon owned state
    state                  TEXT        NOT NULL DEFAULT 'online',
    last_ip                TEXT,
    last_ipv6              TEXT,
    last_interface         TEXT,
    first_seen_at          TIMESTAMPTZ NOT NULL,
    last_seen_at           TIMESTAMPTZ NOT NULL,
    baseline               BOOLEAN     NOT NULL DEFAULT FALSE,
    current_ap             TEXT,
    current_location       TEXT,
    -- the sync contract: the daemon raises sync_state to 'dirty' on create or
    -- change and the plugin's cron sync lowers it to 'clean'. The kernel→daemon
    -- write-back may not write it, which is what stops an admin edit from
    -- triggering a sync pass (DESIGN.md Decision 4).
    sync_state             TEXT        NOT NULL DEFAULT 'dirty',
    trovato_item_id        UUID,
    -- epoch twins, generated and unwritable
    first_seen_at_epoch    BIGINT GENERATED ALWAYS AS
        (EXTRACT(EPOCH FROM (first_seen_at AT TIME ZONE 'UTC'))::bigint) STORED,
    last_seen_at_epoch     BIGINT GENERATED ALWAYS AS
        (EXTRACT(EPOCH FROM (last_seen_at AT TIME ZONE 'UTC'))::bigint) STORED
);

-- ---------------------------------------------------------------------------
-- Timelines
-- ---------------------------------------------------------------------------
-- Presence, location and addressing are the device's history, not standalone
-- entities: they are rendered onto the device page by tap_item_view and are
-- never Items. Declared as read-only record types so an operator can still
-- inspect them without a SQL client.
--
-- `is_summary` marks a compacted day rather than an observed session. The
-- device page's timelines exclude those rows: the page counts sessions and
-- reports a longest session, and a compacted day is neither.

CREATE TABLE IF NOT EXISTS ng_presence (
    id                BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    device_id         BIGINT      NOT NULL REFERENCES ng_devices (id) ON DELETE CASCADE,
    interface         TEXT,
    ip                TEXT,
    started_at        TIMESTAMPTZ NOT NULL,
    ended_at          TIMESTAMPTZ,
    is_summary        BOOLEAN     NOT NULL DEFAULT FALSE,
    observation_count BIGINT      NOT NULL DEFAULT 1,
    started_at_epoch  BIGINT GENERATED ALWAYS AS
        (EXTRACT(EPOCH FROM (started_at AT TIME ZONE 'UTC'))::bigint) STORED,
    ended_at_epoch    BIGINT GENERATED ALWAYS AS
        (EXTRACT(EPOCH FROM (ended_at AT TIME ZONE 'UTC'))::bigint) STORED
);

-- At most one open session per device. Guarded on ownership: see the note above
-- `ng_devices`. On a shared install the daemon owns this table and has already
-- created its own equivalent index under its own name, and `CREATE INDEX IF NOT
-- EXISTS` is not a way out — Postgres resolves the table and checks ownership
-- before it looks at the index name, so the statement raises `must be owner of
-- table ng_presence` whether or not the index exists.
DO $$
BEGIN
    IF pg_catalog.pg_has_role(
        current_user,
        (SELECT relowner FROM pg_catalog.pg_class WHERE oid = 'ng_presence'::regclass),
        'USAGE'
    ) THEN
        CREATE UNIQUE INDEX IF NOT EXISTS uniq_ng_presence_open
            ON ng_presence (device_id) WHERE ended_at IS NULL AND is_summary = FALSE;
    END IF;
END $$;

CREATE TABLE IF NOT EXISTS ng_location_history (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    device_id        BIGINT      NOT NULL REFERENCES ng_devices (id) ON DELETE CASCADE,
    ap_name          TEXT,
    location         TEXT        NOT NULL,
    started_at       TIMESTAMPTZ NOT NULL,
    ended_at         TIMESTAMPTZ,
    is_summary       BOOLEAN     NOT NULL DEFAULT FALSE,
    started_at_epoch BIGINT GENERATED ALWAYS AS
        (EXTRACT(EPOCH FROM (started_at AT TIME ZONE 'UTC'))::bigint) STORED,
    ended_at_epoch   BIGINT GENERATED ALWAYS AS
        (EXTRACT(EPOCH FROM (ended_at AT TIME ZONE 'UTC'))::bigint) STORED
);

-- At most one open stay per device. Guarded on ownership for the same reason as
-- `uniq_ng_presence_open` above.
DO $$
BEGIN
    IF pg_catalog.pg_has_role(
        current_user,
        (SELECT relowner FROM pg_catalog.pg_class WHERE oid = 'ng_location_history'::regclass),
        'USAGE'
    ) THEN
        CREATE UNIQUE INDEX IF NOT EXISTS uniq_ng_location_open
            ON ng_location_history (device_id) WHERE ended_at IS NULL AND is_summary = FALSE;
    END IF;
END $$;

CREATE TABLE IF NOT EXISTS ng_ip_history (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    device_id        BIGINT      NOT NULL REFERENCES ng_devices (id) ON DELETE CASCADE,
    ip               TEXT        NOT NULL,
    interface        TEXT,
    first_seen       TIMESTAMPTZ NOT NULL,
    last_seen        TIMESTAMPTZ NOT NULL,
    first_seen_epoch BIGINT GENERATED ALWAYS AS
        (EXTRACT(EPOCH FROM (first_seen AT TIME ZONE 'UTC'))::bigint) STORED,
    last_seen_epoch  BIGINT GENERATED ALWAYS AS
        (EXTRACT(EPOCH FROM (last_seen AT TIME ZONE 'UTC'))::bigint) STORED,
    UNIQUE (device_id, ip)
);

-- ---------------------------------------------------------------------------
-- Events
-- ---------------------------------------------------------------------------
-- A lightweight record, not an Item (DESIGN.md Decision 2): ~300 rows a day for
-- 90 days, never edited, deleted wholesale on a retention timer. Items would
-- mean 27,000 revisioned rows and 300 delete-item host calls a day.
--
-- `details` is a JSONB object, not a string containing JSON: the `db` host
-- decodes JSONB, so the plugin reads it as an object.
CREATE TABLE IF NOT EXISTS ng_events (
    id              BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    device_id       BIGINT      REFERENCES ng_devices (id) ON DELETE SET NULL,
    event_type      TEXT        NOT NULL,
    "timestamp"     TIMESTAMPTZ NOT NULL,
    details         JSONB       NOT NULL DEFAULT '{}'::jsonb,
    notified        BOOLEAN     NOT NULL DEFAULT FALSE,
    sync_state      TEXT        NOT NULL DEFAULT 'dirty',
    timestamp_epoch BIGINT GENERATED ALWAYS AS
        (EXTRACT(EPOCH FROM ("timestamp" AT TIME ZONE 'UTC'))::bigint) STORED
);

-- ---------------------------------------------------------------------------
-- People
-- ---------------------------------------------------------------------------
-- Derived, one-directional: an ng_person Item is the source of truth and
-- tap_item_insert / tap_item_update / tap_item_delete mirror it here. The daemon
-- reads this table (and ng_devices.owner_item_id) to answer "whose device is
-- this" without ever touching the kernel's `item` table (DESIGN.md Decision 3).
-- The daemon writes the presence columns back: state, current_location and the
-- two arrival timestamps are its, and the mirror upsert never names them.
CREATE TABLE IF NOT EXISTS ng_people (
    item_id          UUID PRIMARY KEY,
    name             TEXT    NOT NULL,
    notes            TEXT,
    notify_arrive    BOOLEAN NOT NULL DEFAULT FALSE,
    notify_depart    BOOLEAN NOT NULL DEFAULT FALSE,
    state            TEXT    NOT NULL DEFAULT 'away',
    current_location TEXT,
    last_arrived_at  TIMESTAMPTZ,
    last_departed_at TIMESTAMPTZ
);

-- ---------------------------------------------------------------------------
-- Plugin state
-- ---------------------------------------------------------------------------
-- The plugin's own scratch space: the sync cursor and the retention setting.
-- The daemon does not know about this table. Separate from every daemon table so
-- a `DROP` of the daemon's schema during a daemon upgrade cannot take the
-- plugin's bookkeeping with it.
CREATE TABLE IF NOT EXISTS ng_state (
    key    TEXT PRIMARY KEY,
    value  TEXT NOT NULL
);
