-- netgraspd's notify tables (its schema V4), guarded, so the Alerts page works
-- on an install with no daemon. Forward-only; no rollback. Safe to run twice.
--
-- The same rule as 001: the daemon owns these tables and creates them itself;
-- this is a byte-for-byte copy of its DDL behind IF NOT EXISTS, and
-- `the_plugin_migration_is_a_faithful_copy_of_the_daemons_schema` compares the
-- two column by column against tests/fixtures/daemon_schema.sql. netgraspd's own
-- V4 is guarded too, so a site that upgrades the plugin before the daemon does
-- not stop the daemon migrating.
--
-- Neither table ever holds the ntfy topic, the server or the token. That stays
-- in netgraspd's config, which the plugin cannot read.
--
-- ng_notify_status: one row, daemon owned in full. Whether delivery is
-- configured, by which transport, and how the most recent attempt went.
CREATE TABLE IF NOT EXISTS ng_notify_status (
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

-- ng_notify_requests: "send me a test alert". The plugin inserts a row (Send
-- test on /alerts) and never updates one; the daemon answers on its reconcile
-- tick and writes handled_at and outcome (sent, failed, not_configured).
CREATE TABLE IF NOT EXISTS ng_notify_requests (
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
-- Guarded on ownership, as 001's indexes are. On a shared install the daemon
-- owns this table and has already created this index, and `CREATE INDEX IF NOT
-- EXISTS` is no way out: Postgres checks ownership of the table before it looks
-- at the index name, so a kernel connecting as any other role would fail here
-- with `must be owner of table ng_notify_requests`, and the install with it.
DO $$
BEGIN
    IF pg_catalog.pg_has_role(
        current_user,
        (SELECT relowner FROM pg_catalog.pg_class WHERE oid = 'ng_notify_requests'::regclass),
        'USAGE'
    ) THEN
        CREATE INDEX IF NOT EXISTS ng_notify_requests_pending_idx ON ng_notify_requests (id)
            WHERE handled_at IS NULL;
    END IF;
END $$;
