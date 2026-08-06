-- The netgrasp daemon's schema, milestones 1 through 3, exactly as the daemon
-- creates it.
--
-- This is the canonical text. It is NOT the plugin's migration: the plugin ships
-- a guarded copy in plugins/netgrasp/migrations/001_netgrasp_schema.sql so an
-- install with no daemon can still enable the plugin, and
-- `the_plugin_migration_is_a_faithful_copy_of_the_daemons_schema` compares the
-- two column by column. This file is what the plugin's queries are run against,
-- because the daemon's schema is what they will meet in production.
--
-- No `IF NOT EXISTS` guards, on purpose: the test applies it into a scratch
-- schema of its own, and a silent no-op over an existing table is exactly the
-- failure mode this fixture exists to rule out.
--
-- ng_device_signals also exists daemon side, holding every raw identity signal
-- ever seen. The plugin does not read it and does not declare it, so it is not
-- reproduced here. ng_state is the plugin's own scratch table and is not the
-- daemon's, so it is not here either.

CREATE TABLE ng_devices (
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
    -- the sync contract
    sync_state             TEXT        NOT NULL DEFAULT 'dirty',
    trovato_item_id        UUID,
    -- epoch twins, generated and unwritable
    first_seen_at_epoch    BIGINT GENERATED ALWAYS AS
        (EXTRACT(EPOCH FROM (first_seen_at AT TIME ZONE 'UTC'))::bigint) STORED,
    last_seen_at_epoch     BIGINT GENERATED ALWAYS AS
        (EXTRACT(EPOCH FROM (last_seen_at AT TIME ZONE 'UTC'))::bigint) STORED
);

CREATE TABLE ng_presence (
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

CREATE TABLE ng_events (
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

CREATE TABLE ng_ip_history (
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

CREATE TABLE ng_location_history (
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

CREATE TABLE ng_people (
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

-- At most one open row per device, on both timelines that have one.
CREATE UNIQUE INDEX uniq_ng_presence_open
    ON ng_presence (device_id) WHERE ended_at IS NULL AND is_summary = FALSE;
CREATE UNIQUE INDEX uniq_ng_location_open
    ON ng_location_history (device_id) WHERE ended_at IS NULL AND is_summary = FALSE;
