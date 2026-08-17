-- A small home network, written the way the daemon writes one.
--
-- WHY THIS EXISTS
-- The daemon needs a real LAN and packet capture, so it cannot run in CI or on a
-- laptop pointed at a scratch database. Every page in this repository is a
-- listing over the daemon's tables, and a listing with no rows proves almost
-- nothing: an empty table and a template that silently renders nothing look the
-- same. This file puts enough rows in to make every page's real shape visible,
-- and to make a row count checkable against the database.
--
-- It writes ONLY the daemon's ng_* tables plus the two `ng_person` Items that
-- give /people something to list. Nothing here is needed to install or run the
-- plugin; it is verification and demonstration data.
--
-- Idempotent: rerunning replaces the demo rows rather than doubling them. It
-- deletes by the MAC prefix and the fixed Item ids it owns, so it will not touch
-- rows a real daemon wrote alongside it.
--
-- Usage:
--   psql "$DATABASE_URL" -f scripts/seed-demo.sql

BEGIN;

-- ---------------------------------------------------------------------------
-- Clear the previous run
-- ---------------------------------------------------------------------------
-- The demo owns the 02:00:5e:… locally-administered range, which no real NIC
-- uses. Events, presence, location and IP history cascade off ng_devices.
DELETE FROM ng_devices WHERE mac LIKE '02:00:5e:%';
DELETE FROM ng_people  WHERE item_id IN (
    '5eed0000-0000-4000-8000-000000000001',
    '5eed0000-0000-4000-8000-000000000002',
    '5eed0000-0000-4000-8000-000000000003'
);
DELETE FROM item WHERE id IN (
    '5eed0000-0000-4000-8000-000000000001',
    '5eed0000-0000-4000-8000-000000000002',
    '5eed0000-0000-4000-8000-000000000003'
);

-- ---------------------------------------------------------------------------
-- Three people, as Items, mirrored into ng_people
-- ---------------------------------------------------------------------------
-- People are Items (DESIGN.md Decision 3) and ng_people is the derived mirror
-- the daemon reads. In production the plugin's tap_item_insert writes the mirror
-- when the Item is created; here both sides are written directly, because the
-- point is to have the pages populated, not to exercise the tap.
--
-- `fields` uses the bare-scalar shape an admin form produces rather than the
-- `{"value": …}` wrapper a plugin write produces. Both are live in the tree, and
-- the person template reads either — seeding the admin shape is the half that
-- was silently rendering blank before that template existed.
INSERT INTO item (id, type, title, author_id, status, created, changed, fields)
SELECT
    v.id::uuid, 'ng_person', v.title,
    (SELECT id FROM users ORDER BY created LIMIT 1),
    1,
    EXTRACT(EPOCH FROM NOW())::bigint - v.age,
    EXTRACT(EPOCH FROM NOW())::bigint - v.age,
    v.fields::jsonb
FROM (VALUES
    ('5eed0000-0000-4000-8000-000000000001', 'Jamie',  86400 * 30,
     '{"field_notes": "Works from the studio most days.", "field_notify_arrive": true, "field_notify_depart": true}'),
    ('5eed0000-0000-4000-8000-000000000002', 'Aurora', 86400 * 25,
     '{"field_notes": "", "field_notify_arrive": true, "field_notify_depart": false}'),
    ('5eed0000-0000-4000-8000-000000000003', 'Arlo',   86400 * 25,
     '{"field_notes": "Tablet is on the guest VLAN.", "field_notify_arrive": false, "field_notify_depart": false}')
) AS v(id, title, age, fields);

INSERT INTO ng_people (item_id, name, notes, notify_arrive, notify_depart, state, current_location, last_arrived_at)
VALUES
    ('5eed0000-0000-4000-8000-000000000001', 'Jamie',  'Works from the studio most days.', TRUE,  TRUE,  'home', 'Studio',      NOW() - INTERVAL '3 hours'),
    ('5eed0000-0000-4000-8000-000000000002', 'Aurora', NULL,                               TRUE,  FALSE, 'home', 'Living room', NOW() - INTERVAL '40 minutes'),
    ('5eed0000-0000-4000-8000-000000000003', 'Arlo',   'Tablet is on the guest VLAN.',     FALSE, FALSE, 'away', NULL,          NOW() - INTERVAL '2 days');

-- ---------------------------------------------------------------------------
-- Twelve devices: four states, seven types, three owners
-- ---------------------------------------------------------------------------
-- Deliberate coverage, because each of these is a branch in a template:
--   * the four `state` values the device table colours
--   * a device with no type, so the Type cell renders its dash rather than a
--     link to /devices/type with an empty facet
--   * a device with no IPv4, ditto for the IP cell
--   * an unowned device, so the Owner cell renders its dash
--   * a device whose only name is its MAC, which is the last rung of the naming
--     ladder and the one that fires when a `default` filter would not
--   * two devices already linked to a Trovato Item, so the name renders as a
--     link, and ten that are not, which is the state before the first cron sync
INSERT INTO ng_devices (
    mac, display_name, notes, hidden, notify, owner_item_id,
    resolved_name, identity_source, identity_confidence, hostname, mdns_name,
    vendor, device_type, device_type_confidence, os_family,
    state, last_ip, last_ipv6, last_interface,
    first_seen_at, last_seen_at, baseline, current_ap, current_location,
    sync_state, trovato_item_id
) VALUES
    ('02:00:5e:00:00:01', 'Jamie''s laptop', NULL, FALSE, TRUE, '5eed0000-0000-4000-8000-000000000001',
     'jamie-mbp', 'mdns', 0.95, 'jamie-mbp.local', 'jamie-mbp', 'Apple, Inc.', 'laptop', 0.92, 'macOS',
     'online', '10.0.1.24', 'fd00::24', 'eth0',
     NOW() - INTERVAL '400 days', NOW() - INTERVAL '20 seconds', TRUE, 'Studio AP', 'Studio',
     'clean', NULL),

    ('02:00:5e:00:00:02', NULL, NULL, FALSE, TRUE, '5eed0000-0000-4000-8000-000000000001',
     'jamie-phone', 'dhcp', 0.80, 'jamie-phone', NULL, 'Apple, Inc.', 'phone', 0.88, 'iOS',
     'online', '10.0.1.31', NULL, 'eth0',
     NOW() - INTERVAL '380 days', NOW() - INTERVAL '90 seconds', TRUE, 'Studio AP', 'Studio',
     'clean', NULL),

    ('02:00:5e:00:00:03', NULL, NULL, FALSE, TRUE, '5eed0000-0000-4000-8000-000000000002',
     NULL, NULL, NULL, 'aurora-tablet', NULL, 'Samsung Electronics', 'tablet', 0.71, 'Android',
     'online', '10.0.1.55', NULL, 'eth0',
     NOW() - INTERVAL '210 days', NOW() - INTERVAL '4 minutes', FALSE, 'Living room AP', 'Living room',
     'clean', NULL),

    ('02:00:5e:00:00:04', NULL, NULL, FALSE, TRUE, '5eed0000-0000-4000-8000-000000000003',
     NULL, NULL, NULL, NULL, NULL, 'Amazon Technologies', 'tablet', 0.64, 'Android',
     'offline', '10.0.2.18', NULL, 'eth0',
     NOW() - INTERVAL '190 days', NOW() - INTERVAL '2 days', FALSE, NULL, NULL,
     'clean', NULL),

    -- The router: baseline infrastructure, always on, no owner.
    ('02:00:5e:00:00:05', 'Gateway', 'Do not touch.', FALSE, FALSE, NULL,
     'gateway', 'reverse-dns', 0.99, 'gateway.lan', NULL, 'Ubiquiti Inc.', 'router', 0.98, 'Linux',
     'online', '10.0.1.1', 'fd00::1', 'eth0',
     NOW() - INTERVAL '700 days', NOW() - INTERVAL '5 seconds', TRUE, NULL, 'Rack',
     'clean', NULL),

    ('02:00:5e:00:00:06', NULL, NULL, FALSE, TRUE, NULL,
     'printer', 'mdns', 0.90, 'printer.local', 'HP-LaserJet', 'HP Inc.', 'printer', 0.94, NULL,
     'idle', '10.0.1.40', NULL, 'eth0',
     NOW() - INTERVAL '320 days', NOW() - INTERVAL '25 minutes', TRUE, NULL, 'Office',
     'clean', NULL),

    ('02:00:5e:00:00:07', NULL, NULL, FALSE, TRUE, NULL,
     NULL, NULL, NULL, NULL, NULL, 'Espressif Inc.', 'iot', 0.55, NULL,
     'idle', '10.0.1.71', NULL, 'eth0',
     NOW() - INTERVAL '95 days', NOW() - INTERVAL '18 minutes', FALSE, NULL, 'Kitchen',
     'clean', NULL),

    -- Nothing but a MAC: no display name, no resolved name, no hostname. The
    -- last rung of the naming ladder.
    ('02:00:5e:00:00:08', NULL, NULL, FALSE, TRUE, NULL,
     NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
     'unknown', NULL, NULL, NULL,
     NOW() - INTERVAL '2 hours', NOW() - INTERVAL '2 hours', FALSE, NULL, NULL,
     'clean', NULL),

    ('02:00:5e:00:00:09', 'Living room TV', NULL, FALSE, TRUE, NULL,
     'living-room-tv', 'mdns', 0.85, NULL, 'LivingRoomTV', 'LG Electronics', 'tv', 0.90, 'webOS',
     'online', '10.0.1.62', NULL, 'eth0',
     NOW() - INTERVAL '260 days', NOW() - INTERVAL '3 minutes', TRUE, 'Living room AP', 'Living room',
     'clean', NULL),

    ('02:00:5e:00:00:0a', NULL, NULL, FALSE, TRUE, '5eed0000-0000-4000-8000-000000000002',
     'aurora-laptop', 'dhcp', 0.75, 'aurora-laptop', NULL, 'Dell Inc.', 'laptop', 0.86, 'Windows',
     'online', '10.0.1.28', NULL, 'eth0',
     NOW() - INTERVAL '150 days', NOW() - INTERVAL '45 seconds', FALSE, 'Living room AP', 'Living room',
     'clean', NULL),

    ('02:00:5e:00:00:0b', NULL, NULL, FALSE, TRUE, NULL,
     'nas', 'reverse-dns', 0.97, 'nas.lan', NULL, 'Synology Inc.', 'server', 0.95, 'Linux',
     'online', '10.0.1.10', 'fd00::10', 'eth0',
     NOW() - INTERVAL '520 days', NOW() - INTERVAL '10 seconds', TRUE, NULL, 'Rack',
     'clean', NULL),

    -- An unexpected arrival, still on the network. This is the row the security
    -- page's events point at.
    ('02:00:5e:00:00:0c', NULL, NULL, FALSE, TRUE, NULL,
     NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
     'online', '10.0.1.201', NULL, 'eth0',
     NOW() - INTERVAL '35 minutes', NOW() - INTERVAL '30 seconds', FALSE, 'Guest AP', 'Guest',
     'clean', NULL);

-- ---------------------------------------------------------------------------
-- Two devices already carry a Trovato Item
-- ---------------------------------------------------------------------------
-- The device table renders a name as a LINK only once the cron sync has minted
-- the device's Item; until then trovato_item_id is null and the record id is a
-- bigint the uuid-keyed item route cannot open. Both branches need to be on the
-- page, so two devices get one.
INSERT INTO item (id, type, title, author_id, status, created, changed, fields)
SELECT
    v.id::uuid, 'ng_device', v.title,
    (SELECT id FROM users ORDER BY created LIMIT 1),
    1,
    EXTRACT(EPOCH FROM NOW())::bigint - 86400,
    EXTRACT(EPOCH FROM NOW())::bigint - 86400,
    v.fields::jsonb
FROM (VALUES
    ('5eed0000-0000-4000-8000-0000000000d1', 'Jamie''s laptop',
     '{"field_mac": "02:00:5e:00:00:01", "field_owner": "5eed0000-0000-4000-8000-000000000001", "field_notes": "", "field_hidden": false, "field_notify": true}'),
    ('5eed0000-0000-4000-8000-0000000000d2', 'Gateway',
     '{"field_mac": "02:00:5e:00:00:05", "field_owner": "", "field_notes": "Do not touch.", "field_hidden": false, "field_notify": false}')
) AS v(id, title, fields)
ON CONFLICT (id) DO UPDATE SET title = EXCLUDED.title, fields = EXCLUDED.fields;

UPDATE ng_devices SET trovato_item_id = '5eed0000-0000-4000-8000-0000000000d1'
WHERE mac = '02:00:5e:00:00:01';
UPDATE ng_devices SET trovato_item_id = '5eed0000-0000-4000-8000-0000000000d2'
WHERE mac = '02:00:5e:00:00:05';

-- ---------------------------------------------------------------------------
-- Events, including every security type the plugin flags
-- ---------------------------------------------------------------------------
-- The six security types in netgrasp_core::model::SECURITY_EVENT_TYPES all
-- appear, so /events/security lists each and /events shows each flagged in the
-- middle of ordinary traffic — which is the behaviour the shared event table
-- exists for. Ordinary types are interleaved by timestamp rather than grouped, so
-- the flagging is being read against a realistic log rather than a tidy one.
INSERT INTO ng_events (device_id, event_type, "timestamp", details, notified, sync_state)
SELECT d.id, v.event_type, NOW() - (v.ago || ' minutes')::interval, v.details::jsonb, FALSE, 'clean'
FROM (VALUES
    ('02:00:5e:00:00:0c', 'new_device',       35, '{"ip": "10.0.1.201", "interface": "eth0"}'),
    ('02:00:5e:00:00:0c', 'arp_scan',         33, '{"targets": 214, "window_seconds": 60}'),
    ('02:00:5e:00:00:01', 'device_online',    32, '{"ip": "10.0.1.24"}'),
    ('02:00:5e:00:00:0c', 'gratuitous_arp',   30, '{"count": 9, "claimed_ip": "10.0.1.1"}'),
    ('02:00:5e:00:00:05', 'arp_spoof',        29, '{"claimed_ip": "10.0.1.1", "by_mac": "02:00:5e:00:00:0c"}'),
    ('02:00:5e:00:00:03', 'device_online',    28, '{"ip": "10.0.1.55"}'),
    ('02:00:5e:00:00:0c', 'ip_conflict',      26, '{"ip": "10.0.1.55", "other_mac": "02:00:5e:00:00:03"}'),
    ('02:00:5e:00:00:06', 'device_idle',      25, '{}'),
    ('02:00:5e:00:00:0c', 'rogue_dhcp',       22, '{"offered": "10.0.9.0/24", "server_ip": "10.0.1.201"}'),
    ('02:00:5e:00:00:07', 'device_idle',      18, '{}'),
    ('02:00:5e:00:00:03', 'ip_changed',       15, '{"from": "10.0.1.54", "to": "10.0.1.55"}'),
    ('02:00:5e:00:00:0a', 'identity_change',  12, '{"from": "aurora-pc", "to": "aurora-laptop", "source": "dhcp"}'),
    ('02:00:5e:00:00:09', 'device_online',     9, '{"ip": "10.0.1.62"}'),
    ('02:00:5e:00:00:0a', 'location_changed',  7, '{"from": "Studio", "to": "Living room"}'),
    ('02:00:5e:00:00:04', 'device_offline',    6, '{"last_ip": "10.0.2.18"}'),
    ('02:00:5e:00:00:02', 'device_online',     5, '{"ip": "10.0.1.31"}'),
    ('02:00:5e:00:00:0b', 'device_online',     3, '{"ip": "10.0.1.10"}'),
    ('02:00:5e:00:00:01', 'location_changed',  2, '{"from": "Living room", "to": "Studio"}'),
    ('02:00:5e:00:00:08', 'new_device',        1, '{"interface": "eth0"}')
) AS v(mac, event_type, ago, details)
JOIN ng_devices d ON d.mac = v.mac;

-- ---------------------------------------------------------------------------
-- One device's history, for the device page's timelines
-- ---------------------------------------------------------------------------
-- Jamie's laptop, because it is the row with an Item and therefore the one whose
-- device page is reachable by clicking a name. One open span on each timeline
-- (ended_at null, is_summary false) plus closed ones behind it: the partial
-- unique indexes allow exactly one open row per device, which is the invariant
-- the daemon maintains and this seed has to respect.
INSERT INTO ng_presence (device_id, interface, ip, started_at, ended_at, is_summary, observation_count)
SELECT d.id, 'eth0', v.ip, NOW() - (v.from_ago || ' hours')::interval,
       CASE WHEN v.to_ago IS NULL THEN NULL ELSE NOW() - (v.to_ago || ' hours')::interval END,
       FALSE, v.observations
FROM (VALUES
    ('10.0.1.24', 3,  NULL, 8100),
    ('10.0.1.24', 30, 26,   9400),
    ('10.0.1.19', 54, 49,   7200)
) AS v(ip, from_ago, to_ago, observations)
JOIN ng_devices d ON d.mac = '02:00:5e:00:00:01';

INSERT INTO ng_location_history (device_id, ap_name, location, started_at, ended_at, is_summary)
SELECT d.id, v.ap, v.location, NOW() - (v.from_ago || ' hours')::interval,
       CASE WHEN v.to_ago IS NULL THEN NULL ELSE NOW() - (v.to_ago || ' hours')::interval END,
       FALSE
FROM (VALUES
    ('Studio AP',      'Studio',      3,  NULL),
    ('Living room AP', 'Living room', 9,  3),
    ('Studio AP',      'Studio',      30, 26)
) AS v(ap, location, from_ago, to_ago)
JOIN ng_devices d ON d.mac = '02:00:5e:00:00:01';

INSERT INTO ng_ip_history (device_id, ip, interface, first_seen, last_seen)
SELECT d.id, v.ip, 'eth0',
       NOW() - (v.first_ago || ' days')::interval,
       NOW() - (v.last_ago || ' hours')::interval
FROM (VALUES
    ('10.0.1.24', 40, 0),
    ('10.0.1.19', 90, 49)
) AS v(ip, first_ago, last_ago)
JOIN ng_devices d ON d.mac = '02:00:5e:00:00:01'
ON CONFLICT (device_id, ip) DO NOTHING;

COMMIT;

-- What was written, so a caller can check a page's row count against it without
-- writing the query again.
SELECT 'devices'          AS listing, count(*) FROM ng_devices
UNION ALL SELECT 'devices online',   count(*) FROM ng_devices WHERE state = 'online'
UNION ALL SELECT 'devices idle',     count(*) FROM ng_devices WHERE state = 'idle'
UNION ALL SELECT 'devices offline',  count(*) FROM ng_devices WHERE state = 'offline'
UNION ALL SELECT 'devices owned',    count(*) FROM ng_devices WHERE owner_item_id IS NOT NULL
UNION ALL SELECT 'events',           count(*) FROM ng_events
UNION ALL SELECT 'security events',  count(*) FROM ng_events
    WHERE event_type IN ('arp_scan', 'arp_spoof', 'gratuitous_arp', 'identity_change', 'ip_conflict', 'rogue_dhcp')
UNION ALL SELECT 'people',           count(*) FROM item WHERE type = 'ng_person' AND status = 1
ORDER BY 1;
