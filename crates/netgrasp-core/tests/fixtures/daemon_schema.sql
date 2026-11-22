-- The netgrasp daemon's schema as of its V8 (netgraspd#36, event indexes):
-- V1 through V3, the notify tables of V4, V5's person epochs, event person and
-- daemon status, V6's notify default, V7's presence columns and V8's three
-- partial indexes on ng_events, exactly as the daemon creates them. Until U18
-- this file stopped at V4, with V4's `notify DEFAULT TRUE`.
--
-- Not every daemon index is here: V1's time and device indexes and V2's
-- security index on ng_events are left to the tests that need them, which
-- create them in the daemon's words. V8's are here because every page they
-- serve has to find them in the daemon's real schema.
--
-- Column order within a table is the order the faithful-copy test compares, so
-- the columns later versions added with `ALTER TABLE … ADD COLUMN` come last,
-- in the order they were added, which is where the daemon puts them too.
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
    -- V6: off by default.
    notify                 BOOLEAN     NOT NULL DEFAULT FALSE,
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
        (EXTRACT(EPOCH FROM (last_seen_at AT TIME ZONE 'UTC'))::bigint) STORED,
    -- V7, user owned: NULL decides by device type (phone and wearable count),
    -- TRUE counts for whether the owner is home, FALSE does not.
    counts_for_presence    BOOLEAN
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
        (EXTRACT(EPOCH FROM ("timestamp" AT TIME ZONE 'UTC'))::bigint) STORED,
    -- V5: the person a person_arrived / person_departed event is about.
    person_item_id  UUID
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
    -- V7: 'home', 'away' or 'untracked'; everyone starts untracked.
    state            TEXT    NOT NULL DEFAULT 'untracked',
    current_location TEXT,
    last_arrived_at  TIMESTAMPTZ,
    last_departed_at TIMESTAMPTZ,
    -- V5: epoch twins.
    last_arrived_at_epoch  BIGINT GENERATED ALWAYS AS
        (EXTRACT(EPOCH FROM (last_arrived_at AT TIME ZONE 'UTC'))::bigint) STORED,
    last_departed_at_epoch BIGINT GENERATED ALWAYS AS
        (EXTRACT(EPOCH FROM (last_departed_at AT TIME ZONE 'UTC'))::bigint) STORED,
    -- V7, user owned: whether this person's presence is tracked at all.
    track_presence   BOOLEAN NOT NULL DEFAULT FALSE
);

-- At most one open row per device, on both timelines that have one, under the
-- daemon's own names (V1 and V3). The plugin's 001 gives its copies different
-- names, which is how 017 tells them apart.
CREATE UNIQUE INDEX ng_presence_open_idx
    ON ng_presence (device_id) WHERE ended_at IS NULL AND is_summary = FALSE;
CREATE UNIQUE INDEX ng_location_history_open_idx
    ON ng_location_history (device_id) WHERE ended_at IS NULL AND is_summary = FALSE;

-- V5: a person's movements, read by the plugin's movements view (019).
CREATE INDEX ng_events_person_idx ON ng_events (person_item_id, "timestamp" DESC)
    WHERE person_item_id IS NOT NULL;

-- V8: the event pages' indexes. Every event but ip_changed by type and time
-- (Activity, /events/new-devices), the delivered alerts by time ("Recently
-- sent"), and the six security types in one time order (/events/security at
-- any depth, and its total).
CREATE INDEX ng_events_type_time_idx
    ON ng_events (event_type, "timestamp" DESC)
    WHERE event_type <> 'ip_changed';
CREATE INDEX ng_events_notified_time_idx
    ON ng_events ("timestamp" DESC)
    WHERE notified;
CREATE INDEX ng_events_security_time_idx
    ON ng_events ("timestamp" DESC)
    WHERE event_type IN ('arp_scan', 'arp_spoof', 'rogue_dhcp', 'identity_change',
                         'ip_conflict', 'gratuitous_arp');

-- V4: whether alerts can be delivered, and a test somebody asked for. Daemon
-- owned, except that the plugin inserts ng_notify_requests rows. The daemon
-- guards these two with IF NOT EXISTS because the plugin ships a copy; here
-- they are unguarded like everything else, for the reason at the top.
CREATE TABLE ng_notify_status (
    id                     BIGINT      PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    notify_configured      BOOLEAN     NOT NULL,
    notify_backend         TEXT,
    last_delivery_ok       BOOLEAN,
    last_delivery_at       TIMESTAMPTZ,
    updated_at             TIMESTAMPTZ NOT NULL,
    last_delivery_at_epoch BIGINT GENERATED ALWAYS AS
        (EXTRACT(EPOCH FROM (last_delivery_at AT TIME ZONE 'UTC'))::bigint) STORED,
    updated_at_epoch       BIGINT GENERATED ALWAYS AS
        (EXTRACT(EPOCH FROM (updated_at AT TIME ZONE 'UTC'))::bigint) STORED
);

CREATE TABLE ng_notify_requests (
    id                 BIGINT      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind               TEXT        NOT NULL DEFAULT 'test' CHECK (kind = 'test'),
    requested_by       TEXT,
    requested_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    handled_at         TIMESTAMPTZ,
    outcome            TEXT        CHECK (outcome IN ('sent', 'failed', 'not_configured')),
    requested_at_epoch BIGINT GENERATED ALWAYS AS
        (EXTRACT(EPOCH FROM (requested_at AT TIME ZONE 'UTC'))::bigint) STORED,
    handled_at_epoch   BIGINT GENERATED ALWAYS AS
        (EXTRACT(EPOCH FROM (handled_at AT TIME ZONE 'UTC'))::bigint) STORED
);
CREATE INDEX ng_notify_requests_pending_idx ON ng_notify_requests (id)
    WHERE handled_at IS NULL;

-- V5: what the daemon is and how it is configured, one row. V7 adds the
-- install's presence switch (`presence_tracking` in netgraspd's config), which
-- the plugin reads through `ng_presence_tracking()` (023).
CREATE TABLE ng_daemon_status (
    id                BIGINT      PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    version           TEXT        NOT NULL,
    timezone          TEXT        NOT NULL,
    timezone_source   TEXT        NOT NULL,
    started_at        TIMESTAMPTZ NOT NULL,
    updated_at        TIMESTAMPTZ NOT NULL,
    started_at_epoch  BIGINT GENERATED ALWAYS AS
        (EXTRACT(EPOCH FROM (started_at AT TIME ZONE 'UTC'))::bigint) STORED,
    updated_at_epoch  BIGINT GENERATED ALWAYS AS
        (EXTRACT(EPOCH FROM (updated_at AT TIME ZONE 'UTC'))::bigint) STORED,
    presence_tracking BOOLEAN     NOT NULL DEFAULT FALSE
);

-- V5: one row per enricher, daemon owned. The plugin does not read it yet.
CREATE TABLE ng_enrichment_status (
    enricher              TEXT        PRIMARY KEY,
    configured            BOOLEAN     NOT NULL,
    state                 TEXT        NOT NULL,
    polls                 BIGINT      NOT NULL DEFAULT 0,
    failures              BIGINT      NOT NULL DEFAULT 0,
    last_success_at       TIMESTAMPTZ,
    last_failure_at       TIMESTAMPTZ,
    last_error            TEXT,
    updated_at            TIMESTAMPTZ NOT NULL,
    last_success_at_epoch BIGINT GENERATED ALWAYS AS
        (EXTRACT(EPOCH FROM (last_success_at AT TIME ZONE 'UTC'))::bigint) STORED,
    last_failure_at_epoch BIGINT GENERATED ALWAYS AS
        (EXTRACT(EPOCH FROM (last_failure_at AT TIME ZONE 'UTC'))::bigint) STORED,
    updated_at_epoch      BIGINT GENERATED ALWAYS AS
        (EXTRACT(EPOCH FROM (updated_at AT TIME ZONE 'UTC'))::bigint) STORED
);
