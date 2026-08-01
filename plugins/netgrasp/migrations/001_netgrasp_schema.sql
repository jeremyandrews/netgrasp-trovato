-- Netgrasp schema. Forward-only; no rollback.
--
-- These tables are owned by the native netgrasp daemon at runtime — it is the
-- process that watches the LAN and writes what it sees. This migration exists
-- anyway, for two reasons (DESIGN.md Decision 8):
--
--   1. A plugin's effective DB allowlist is (migration-owned ∪ db_tables), and a
--      record type is only admitted over a table inside it. Declaring the tables
--      here and in db_tables makes the allowlist independent of how a future
--      kernel parses CREATE statements.
--   2. An install with no daemon yet must still be able to enable the plugin and
--      show empty pages rather than error.
--
-- Everything below is additive and idempotent — CREATE TABLE IF NOT EXISTS plus
-- ADD COLUMN IF NOT EXISTS — so it converges to the same schema whether the
-- daemon or this migration got there first, and re-running it is a no-op.
--
-- Column ownership is the load-bearing part and is enforced in code, not here:
-- netgrasp_core::columns names the three disjoint sets, and the write-back
-- statement builder generates its SET list from one of them so it cannot name a
-- column from another.

-- ---------------------------------------------------------------------------
-- Devices
-- ---------------------------------------------------------------------------
-- Three writers, three column groups:
--   daemon-owned : mac, hostname, vendor, device_type, os_family, state,
--                  last_ip, current_location, first_seen, last_seen, sync_state
--   user-owned   : display_name, owner_item_id, notes, hidden, notify
--   plugin-owned : trovato_item_id
CREATE TABLE IF NOT EXISTS ng_devices (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    mac               TEXT NOT NULL,
    hostname          TEXT,
    vendor            TEXT,
    device_type       TEXT,
    os_family         TEXT,
    state             TEXT NOT NULL DEFAULT 'new',
    last_ip           TEXT,
    current_location  TEXT,
    first_seen        BIGINT,
    last_seen         BIGINT,
    -- The daemon raises this to 'dirty' on create or change; the plugin's cron
    -- sync lowers it to 'clean'. Nothing else may write it — in particular the
    -- kernel→daemon write-back may not, which is what stops an admin edit from
    -- triggering a sync pass (DESIGN.md Decision 4).
    sync_state        TEXT NOT NULL DEFAULT 'dirty',
    display_name      TEXT,
    owner_item_id     UUID,
    notes             TEXT,
    hidden            BOOLEAN NOT NULL DEFAULT FALSE,
    notify            BOOLEAN NOT NULL DEFAULT FALSE,
    trovato_item_id   UUID
);

-- Added separately so a daemon-created table converges to the same shape. A
-- daemon that predates the plugin has the observation columns and none of the
-- user-owned or link columns.
ALTER TABLE ng_devices ADD COLUMN IF NOT EXISTS display_name     TEXT;
ALTER TABLE ng_devices ADD COLUMN IF NOT EXISTS owner_item_id    UUID;
ALTER TABLE ng_devices ADD COLUMN IF NOT EXISTS notes            TEXT;
ALTER TABLE ng_devices ADD COLUMN IF NOT EXISTS hidden           BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE ng_devices ADD COLUMN IF NOT EXISTS notify           BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE ng_devices ADD COLUMN IF NOT EXISTS trovato_item_id  UUID;
ALTER TABLE ng_devices ADD COLUMN IF NOT EXISTS sync_state       TEXT NOT NULL DEFAULT 'dirty';
ALTER TABLE ng_devices ADD COLUMN IF NOT EXISTS current_location TEXT;
ALTER TABLE ng_devices ADD COLUMN IF NOT EXISTS os_family        TEXT;

-- A MAC is the daemon's device identity, so it must be unique. Created as a
-- unique INDEX rather than a table constraint because it has to be addable to a
-- table the daemon may already have created without one.
CREATE UNIQUE INDEX IF NOT EXISTS uniq_ng_devices_mac ON ng_devices (mac);

-- The sync pass's access path: only dirty rows, newest first. Partial, because
-- the steady state is that almost every row is clean.
CREATE INDEX IF NOT EXISTS idx_ng_devices_dirty
    ON ng_devices (last_seen DESC) WHERE sync_state = 'dirty';
-- The write-back's access path, and the device page's Item→row lookup.
CREATE INDEX IF NOT EXISTS idx_ng_devices_item ON ng_devices (trovato_item_id);
-- The online-devices gather and the by-type / by-owner facets.
CREATE INDEX IF NOT EXISTS idx_ng_devices_state ON ng_devices (state);
CREATE INDEX IF NOT EXISTS idx_ng_devices_type ON ng_devices (device_type);
CREATE INDEX IF NOT EXISTS idx_ng_devices_owner ON ng_devices (owner_item_id);

-- ---------------------------------------------------------------------------
-- People
-- ---------------------------------------------------------------------------
-- Derived, one-directional: an ng_person Item is the source of truth and
-- tap_item_insert / tap_item_update / tap_item_delete mirror it here. The daemon
-- reads this table (and ng_devices.owner_item_id) to answer "whose device is
-- this" without ever touching the kernel's `item` table (DESIGN.md Decision 3).
CREATE TABLE IF NOT EXISTS ng_people (
    item_id        UUID PRIMARY KEY,
    name           TEXT NOT NULL,
    notes          TEXT,
    notify_arrive  BOOLEAN NOT NULL DEFAULT FALSE,
    notify_depart  BOOLEAN NOT NULL DEFAULT FALSE
);

-- ---------------------------------------------------------------------------
-- Events
-- ---------------------------------------------------------------------------
-- A lightweight record, not an Item (DESIGN.md Decision 2): ~300 rows a day for
-- 90 days, never edited, deleted wholesale on a retention timer. Items would
-- mean 27,000 revisioned rows and 300 delete-item host calls a day.
CREATE TABLE IF NOT EXISTS ng_events (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    device_id   UUID,
    event_type  TEXT NOT NULL,
    timestamp   BIGINT NOT NULL,
    details     TEXT
);

-- The event log's default order, and the retention pass's access path.
CREATE INDEX IF NOT EXISTS idx_ng_events_time ON ng_events (timestamp DESC);
-- The per-device event list on a device page.
CREATE INDEX IF NOT EXISTS idx_ng_events_device ON ng_events (device_id, timestamp DESC);
-- The security-events view.
CREATE INDEX IF NOT EXISTS idx_ng_events_type ON ng_events (event_type, timestamp DESC);

-- ---------------------------------------------------------------------------
-- Timelines
-- ---------------------------------------------------------------------------
-- Presence, location and addressing are the device's history, not standalone
-- entities: they are rendered onto the device page by tap_item_view and are
-- never Items. Declared as read-only record types so an operator can still
-- inspect them without a SQL client.

CREATE TABLE IF NOT EXISTS ng_presence (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    device_id   UUID NOT NULL,
    start_time  BIGINT NOT NULL,
    end_time    BIGINT
);
CREATE INDEX IF NOT EXISTS idx_ng_presence_device ON ng_presence (device_id, start_time DESC);

CREATE TABLE IF NOT EXISTS ng_location_history (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    device_id   UUID NOT NULL,
    location    TEXT NOT NULL,
    start_time  BIGINT NOT NULL,
    end_time    BIGINT
);
CREATE INDEX IF NOT EXISTS idx_ng_location_device
    ON ng_location_history (device_id, start_time DESC);

CREATE TABLE IF NOT EXISTS ng_ip_history (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    device_id   UUID NOT NULL,
    ip_address  TEXT NOT NULL,
    first_seen  BIGINT NOT NULL,
    last_seen   BIGINT
);
CREATE INDEX IF NOT EXISTS idx_ng_ip_device ON ng_ip_history (device_id, first_seen DESC);

-- ---------------------------------------------------------------------------
-- Plugin state
-- ---------------------------------------------------------------------------
-- The plugin's own scratch space: the sync cursor and the retention setting.
-- Separate from every daemon table so a `DROP` of the daemon's schema during a
-- daemon upgrade cannot take the plugin's bookkeeping with it.
CREATE TABLE IF NOT EXISTS ng_state (
    key    TEXT PRIMARY KEY,
    value  TEXT NOT NULL
);
