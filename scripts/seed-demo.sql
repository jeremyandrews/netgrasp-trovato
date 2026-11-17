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
-- uses.
--
-- Events go FIRST, and by join, because ng_events.device_id is
-- `ON DELETE SET NULL` and not `ON DELETE CASCADE` — deleting a device orphans
-- its events rather than removing them, which is right for a daemon (the log
-- outlives the device) and wrong for a seed. Deleting the devices first left
-- nineteen orphans behind and the next run added nineteen more: /events showed
-- 38 rows, half of them with an em dash in the Device column, and this file's
-- own claim to be idempotent was false. Presence, location and IP history really
-- do cascade, so they need no statement here.
DELETE FROM ng_events
WHERE device_id IN (SELECT id FROM ng_devices WHERE mac LIKE '02:00:5e:%');
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

    -- An owner id with NO ng_people row behind it. A real state, not a broken
    -- seed: ng_people is written by the plugin's person taps, so a device keeps
    -- naming an owner whose Item has since been deleted. It is the branch where
    -- the owner view resolves no name and the templates fall back to the id chip,
    -- and it is also the row that proves the view's join is a LEFT one — an inner
    -- join would delete this device from every device page.
    ('02:00:5e:00:00:0d', NULL, NULL, FALSE, TRUE, '5eed0000-0000-4000-8000-00000000dead',
     'orphan-owner-laptop', 'dhcp', 0.70, 'orphan-owner-laptop', NULL, 'Lenovo', 'laptop', 0.80, 'Linux',
     'online', '10.0.1.77', NULL, 'eth0',
     NOW() - INTERVAL '60 days', NOW() - INTERVAL '2 minutes', FALSE, 'Office AP', 'Office',
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
-- More of a real network, so that every Devices tab and every part of Home has
-- rows (docs/UI.md): two devices somebody hid, a Raspberry Pi called motivo for
-- the search box, and eight devices that arrived this week that nobody has
-- named or claimed. Eight plus the two above makes ten to sort this week, which
-- is more than the six Home's "Needs attention" shows, so its "more" line has
-- something to say. Four carry a possessive mDNS name, as 22 of 166 do on the
-- live network.
INSERT INTO ng_devices (
    mac, display_name, notes, hidden, notify, owner_item_id,
    resolved_name, identity_source, identity_confidence, hostname, mdns_name,
    vendor, device_type, device_type_confidence, os_family,
    state, last_ip, last_ipv6, last_interface,
    first_seen_at, last_seen_at, baseline, current_ap, current_location,
    sync_state, trovato_item_id
) VALUES
    ('02:00:5e:00:00:0e', 'Hallway smart plug', 'Always on; hidden so it stops cluttering the lists.', TRUE, FALSE, NULL,
     'hs103', 'dhcp', 0.82, 'HS103', NULL, 'TP-Link', 'iot', 0.77, NULL,
     'online', '10.0.1.90', NULL, 'eth0',
     NOW() - INTERVAL '300 days', NOW() - INTERVAL '40 seconds', TRUE, 'Living room AP', 'Living room',
     'clean', NULL),
    ('02:00:5e:00:00:0f', 'Mesh node, upstairs', NULL, TRUE, FALSE, NULL,
     'mesh-upstairs', 'reverse-dns', 0.96, 'mesh-upstairs.lan', NULL, 'Ubiquiti Inc.', 'router', 0.97, 'Linux',
     'online', '10.0.1.2', NULL, 'eth0',
     NOW() - INTERVAL '640 days', NOW() - INTERVAL '8 seconds', TRUE, NULL, 'Rack',
     'clean', NULL),
    ('02:00:5e:00:00:10', NULL, NULL, FALSE, TRUE, NULL,
     'motivo', 'mdns', 0.93, 'motivo.local', 'motivo', 'Raspberry Pi Trading', 'server', 0.81, 'Linux',
     'online', '10.0.1.12', 'fd00::12', 'eth0',
     NOW() - INTERVAL '45 days', NOW() - INTERVAL '15 seconds', TRUE, NULL, 'Office',
     'clean', NULL),
    ('02:00:5e:00:00:11', NULL, NULL, FALSE, TRUE, NULL,
     NULL, 'mdns', 0.88, 'Jamies-iPad', 'Jamie''s iPad', 'Apple, Inc.', 'tablet', 0.90, 'iPadOS',
     'online', '10.0.1.103', NULL, 'eth0',
     NOW() - INTERVAL '3 hours', NOW() - INTERVAL '1 minute', FALSE, 'Studio AP', 'Studio',
     'clean', NULL),
    ('02:00:5e:00:00:12', NULL, NULL, FALSE, TRUE, NULL,
     NULL, 'mdns', 0.86, 'Auroras-Pixel', 'Aurora''s Pixel 8', 'Google, Inc.', 'phone', 0.91, 'Android',
     'online', '10.0.1.104', NULL, 'eth0',
     NOW() - INTERVAL '9 hours', NOW() - INTERVAL '50 seconds', FALSE, 'Living room AP', 'Living room',
     'clean', NULL),
    ('02:00:5e:00:00:13', NULL, NULL, FALSE, TRUE, NULL,
     NULL, 'dhcp', 0.62, 'DESKTOP-7Q2M4KD', NULL, 'Micro-Star INT''L', 'desktop', 0.74, 'Windows',
     'idle', '10.0.1.118', NULL, 'eth0',
     NOW() - INTERVAL '1 day', NOW() - INTERVAL '22 minutes', FALSE, NULL, 'Office',
     'clean', NULL),
    ('02:00:5e:00:00:14', NULL, NULL, FALSE, TRUE, NULL,
     NULL, 'mdns', 0.79, 'Chromecast', 'Living Room TV''s Chromecast', 'Google, Inc.', 'media', 0.85, NULL,
     'online', '10.0.1.121', NULL, 'eth0',
     NOW() - INTERVAL '2 days', NOW() - INTERVAL '2 minutes', FALSE, 'Living room AP', 'Living room',
     'clean', NULL),
    ('02:00:5e:00:00:15', NULL, NULL, FALSE, TRUE, NULL,
     NULL, NULL, NULL, 'esp32-4f1a2c', NULL, 'Espressif Inc.', 'iot', 0.58, NULL,
     'online', '10.0.1.133', NULL, 'eth0',
     NOW() - INTERVAL '3 days', NOW() - INTERVAL '30 seconds', FALSE, NULL, 'Kitchen',
     'clean', NULL),
    ('02:00:5e:00:00:16', NULL, NULL, FALSE, TRUE, NULL,
     NULL, 'mdns', 0.84, 'Arlos-Switch', 'Arlo''s Switch', 'Nintendo Co., Ltd.', 'console', 0.88, NULL,
     'offline', '10.0.2.40', NULL, 'eth0',
     NOW() - INTERVAL '4 days', NOW() - INTERVAL '1 day', FALSE, NULL, NULL,
     'clean', NULL),
    ('02:00:5e:00:00:17', NULL, NULL, FALSE, TRUE, NULL,
     NULL, 'dhcp', 0.70, 'ring-doorbell', NULL, 'Ring LLC', 'camera', 0.83, NULL,
     'online', '10.0.1.140', NULL, 'eth0',
     NOW() - INTERVAL '5 days', NOW() - INTERVAL '45 seconds', FALSE, 'Garage AP', 'Garage',
     'clean', NULL),
    ('02:00:5e:00:00:18', NULL, NULL, FALSE, TRUE, NULL,
     NULL, NULL, NULL, NULL, NULL, 'Murata Manufacturing', NULL, NULL, NULL,
     'idle', '10.0.2.77', NULL, 'eth0',
     NOW() - INTERVAL '6 days', NOW() - INTERVAL '3 hours', FALSE, 'Guest AP', 'Guest',
     'clean', NULL);

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
    ('02:00:5e:00:00:08', 'new_device',        1, '{"interface": "eth0"}'),
    ('02:00:5e:00:00:11', 'new_device',      180, '{"ip": "10.0.1.103", "interface": "eth0"}'),
    ('02:00:5e:00:00:12', 'new_device',      540, '{"ip": "10.0.1.104", "interface": "eth0"}'),
    ('02:00:5e:00:00:13', 'new_device',     1440, '{"ip": "10.0.1.118", "interface": "eth0"}'),
    ('02:00:5e:00:00:11', 'ip_changed',        4, '{"from": "10.0.1.102", "to": "10.0.1.103"}'),
    ('02:00:5e:00:00:14', 'ip_changed',        3, '{"from": "10.0.1.120", "to": "10.0.1.121"}')
) AS v(mac, event_type, ago, details)
JOIN ng_devices d ON d.mac = v.mac;

-- ---------------------------------------------------------------------------
-- Arrivals and departures, for the overview
-- ---------------------------------------------------------------------------
-- The daemon's two person events, with `details` shaped the way it writes them
-- (netgraspd, src/people/mod.rs): the person's name and item id, the device that
-- caused it, and for an arrival where they arrived and the edge they came
-- through. The times agree with the people rows above: Jamie home three hours,
-- Aurora forty minutes, Arlo left this morning after a night home.
--
-- Since netgraspd's schema V4 the daemon also names the device (`device_name`)
-- and, for a departure, when it was last heard from (`last_seen_at`), which is
-- what the alert text and the Alerts page's "Recently sent" read. Arlo's phone
-- fell silent half an hour before the offline timeout declared him gone.
--
-- Arlo's departure also sets `last_departed_at`, which the people insert above
-- leaves null, so the one person who is away has a time to be away since.
UPDATE ng_people SET last_departed_at = NOW() - INTERVAL '5 hours'
WHERE item_id = '5eed0000-0000-4000-8000-000000000003';

INSERT INTO ng_events (device_id, event_type, "timestamp", details, notified, sync_state)
SELECT d.id, v.event_type, NOW() - (v.ago || ' minutes')::interval,
       jsonb_build_object(
           'person', v.person,
           'person_item_id', v.person_item_id,
           'device', v.mac,
           'location', v.location,
           'via', v.via,
           'device_name', COALESCE(d.display_name, d.resolved_name, d.hostname, d.mac),
           'last_seen_at', to_char(
               (NOW() - ((v.ago + CASE WHEN v.event_type = 'person_departed' THEN 30 ELSE 0 END)
                         || ' minutes')::interval) AT TIME ZONE 'UTC',
               'YYYY-MM-DD"T"HH24:MI:SS"Z"')
       ),
       TRUE, 'clean'
FROM (VALUES
    ('02:00:5e:00:00:04', 'person_departed', 300, 'Arlo',   '5eed0000-0000-4000-8000-000000000003', NULL,          'Driveway AP'),
    ('02:00:5e:00:00:02', 'person_arrived',  180, 'Jamie',  '5eed0000-0000-4000-8000-000000000001', 'Studio',      'Driveway AP'),
    ('02:00:5e:00:00:0a', 'person_arrived',   40, 'Aurora', '5eed0000-0000-4000-8000-000000000002', 'Living room', NULL)
) AS v(mac, event_type, ago, person, person_item_id, location, via)
JOIN ng_devices d ON d.mac = v.mac;

-- ---------------------------------------------------------------------------
-- Delivery, for the Alerts page
-- ---------------------------------------------------------------------------
-- netgraspd's status row (its schema V4; the plugin's 015 creates the table on
-- an install with no daemon): ntfy configured, and the last alert, Aurora's
-- arrival forty minutes ago, accepted. DO NOTHING, so a row a real daemon wrote
-- is left as it is.
INSERT INTO ng_notify_status
    (id, notify_configured, notify_backend, last_delivery_ok, last_delivery_at, updated_at)
VALUES (1, TRUE, 'ntfy', TRUE, NOW() - INTERVAL '40 minutes', NOW())
ON CONFLICT (id) DO NOTHING;

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
-- Owned and resolvable to a name, which is what the device pages show. The gap
-- between this and the line above is the dangling-owner row on purpose.
UNION ALL SELECT 'devices named',    count(*) FROM ng_devices d
    JOIN ng_people p ON p.item_id = d.owner_item_id
UNION ALL SELECT 'events',           count(*) FROM ng_events
UNION ALL SELECT 'security events',  count(*) FROM ng_events
    WHERE event_type IN ('arp_scan', 'arp_spoof', 'gratuitous_arp', 'identity_change', 'ip_conflict', 'rogue_dhcp')
UNION ALL SELECT 'people',           count(*) FROM item WHERE type = 'ng_person' AND status = 1
UNION ALL SELECT 'people home',      count(*) FROM ng_people WHERE state = 'home'
UNION ALL SELECT 'devices new this week', count(*) FROM ng_devices
    WHERE first_seen_at > NOW() - INTERVAL '7 days' AND NOT hidden
ORDER BY 1;
