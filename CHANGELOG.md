# Changelog

Netgrasp for Trovato. Entries are grouped by the change, not by the file, and
each one says what was wrong underneath rather than what was edited: a line that
reads "fixed the rename tool" tells the next person nothing about why the next
tool will do it too.

**Versioning.** From 1.0.0 the plugin has a version of its own, semantic over
what an operator meets: the pages and their paths, the permissions, the
assistant scopes, and the migrations. It does not describe the kernel. Each
release records the Trovato it is built against, which is the triple in the
workspace `Cargo.toml` (the `trovato-sdk` revision, the Trovato version and
`KERNEL_API_VERSION`), and the daemon release it pairs with. Before 1.0.0 the
version was the kernel's (0.99.0, then 0.102.0) and nothing was tagged.

**Pairing with the daemon.** A plugin release and a
[netgraspd](https://github.com/jeremyandrews/netgraspd) release pair when they
are built for the same `ng_` schema version. A change to the schema is always a
paired release, and the daemon is upgraded and migrated first. The README's
Compatibility section has the table.

## [Unreleased]

## [1.2.0] - 2026-11-22

Presence settings and a third presence state, "Not tracked", for netgraspd's
schema version 7; one person per name; and event pages, the cron prune and the
assistant reading the daemon's indexes instead of scanning the event log.

| | |
|---|---|
| Pairs with | netgraspd 1.2.0, `ng_` schema version 7 |
| Built against | Trovato 0.102.0, `trovato-sdk` revision `ca76a96`, `KERNEL_API_VERSION` (0, 102) |
| Manifest | `version = "1.2.0"`, `api_version = "0.102"` |
| Runs on | Trovato 0.102.0 or a later 0.x |

Upgrade the daemon to 1.2.0 and run its `migrate` first. Where the kernel's
role does not own the daemon's tables, the table owner grants it the two new
presence columns (`UPDATE (counts_for_presence)` on `ng_devices`, and
`INSERT (track_presence)` and `UPDATE (track_presence)` on `ng_people`) and
creates migration 018's index; the README's role section has the statements.

### Added

**Presence is opt in, and the plugin can say who is tracked and which devices
count.** netgraspd's schema V7 stopped deciding that everybody is home or away.
A person is tracked only when the install's `presence_tracking = true` is set
in netgraspd's config, their own "Track whether they are home" is on, and they
own a device that counts. Every other person is `untracked`, a third state
that is neither home nor away. Before this the plugin had no way to set either
setting, and every page that listed people knew only two states, so an
untracked person would have read as away.

- *Two settings, both user owned, synced the way `notify` and `notify_arrive`
  are.* On a device, `field_counts_for_presence` (By device type, Counts, Does
  not count), written to `ng_devices.counts_for_presence` as NULL, TRUE or
  FALSE. By device type, a phone or a wearable counts and nothing else does. On
  a person, `field_track_presence`, off by default, written to
  `ng_people.track_presence`.
- *Two forms in the row menu.* `/netgrasp/device/presence` offers the three
  choices and says the device's effective value and why ("Counts: phone",
  "Does not count: laptop", "Counts: chosen for this device"). The device page
  shows the same line with a link to the form. `/netgrasp/person/presence` is
  the one switch; the person page links to it.
- *Not tracked, everywhere a person is listed.* Home lists the untracked on a
  line of their own and counts them as neither home nor away. `/people/home`
  lists everybody: home first, then away, then "Not tracked". `/people`'s
  cards say Home, Away or Not tracked. The person page and the Alerts page say
  "Not tracked" and why. The device-shaped Who is home lists only owners the
  daemon has as home, so an untracked person's online laptop no longer makes
  them home.
- *The install's switch.* When netgraspd publishes `presence_tracking = false`
  (`ng_daemon_status.presence_tracking`), Home, `/people` and `/people/home`
  say so in one line naming the config key. With no daemon status to read,
  they say nothing.
- *Migrations.* `022_netgrasp_presence_schema.sql` is the guarded copy of V7's
  two columns. It adds a column only where the column is missing and the role
  owns the table. With no daemon at all it also adds the V5 columns 001's
  copy lacked, and V7's `untracked` default. `023_netgrasp_presence_views.sql`
  adds `ng_presence_tracking()` and three views over the ones the record types
  read (`ng_people_presence_status`, `ng_home_presence`,
  `ng_devices_listed_presence`). These are new views rather than widened ones,
  so 006, 007, 012 and 013 stay re-runnable.
- *A rename no longer drops a person's fields.* The person payload used to
  rebuild `fields` from notes and the two alert flags alone. It now carries
  every existing field forward, so a rename or an alerts edit cannot switch
  tracking off.
- *The daemon schema fixture is up to date.* It stopped at V4, with V4's
  `notify DEFAULT TRUE`. It is now the daemon's schema as of V7: V5's people
  epochs, event person column and index, and `ng_daemon_status` and
  `ng_enrichment_status`; V6's `notify DEFAULT FALSE`; and V7's presence
  columns and `untracked` default.
- *Pairs with* the netgraspd release that carries schema V7. As always, the
  daemon migrates first.

### Fixed

**The event pages and the assistant's event reads answer from the daemon's
indexes instead of scanning the event log.** On the live database (4.1 million
events, 1.1 GB) each of these pages spent 100 to 200 ms per statement reading
all of `ng_events`. Four root causes:

- *The event record type sorted on a column nothing indexes.* `ng_event` maps
  its `timestamp` field to `timestamp_epoch`, the daemon's generated twin, so
  every `ORDER BY` a gather asked for sorted the whole table to show its first
  page. /events, /events/security and /events/device now read record types
  (`ng_event_recent`, `ng_security_event`, `ng_device_event`) whose
  `timestamp` maps to `"timestamp"`, which `ng_events_time_idx` and
  `ng_events_device_idx` serve. The templates still print the epoch twin, so
  every time reads exactly as before. The record admin's listing of
  `ng_event` sorts on `"timestamp"` too.
- *The pager counted every row.* A record gather's total is `COUNT(*)` over
  its relation, and for /events that was the whole log, and for the busiest
  device on /events/device nearly all of it (4.06 million rows). Migration 020
  puts /events over `ng_events_recent`, the newest 10,000 events, and
  /events/device over `ng_device_events`, each device's newest 10,000, both
  read from an index. Where a total reaches the bound the page says "10000+"
  and "showing 100 of the newest 10000"; under it the count is exact and reads
  as it did.
- *The assistant's windows were on the twin.* `SELECT_DEVICE_EVENTS`,
  `SELECT_SECURITY_EVENTS` and `SELECT_SECURITY_EVENT_COUNT` compared their
  cutoff with `timestamp_epoch`, so each read the whole log to keep one day or
  one week of it. They compare `"timestamp" >= to_timestamp($n)` and sort on
  `"timestamp"`.
- *The security total was two statements.* The overview's `security_events`
  and the /events/security pager each counted the six security types with
  their own copy of the list. Both now count one view, `ng_security_events`,
  with one statement, `SELECT count(*) FROM ng_security_events`, which the
  daemon's `ng_events_security_idx` answers by itself. Migration 021 moves the
  three gathers, guarded so a site that edited one keeps its edit.

Medians of nine `EXPLAIN ANALYZE` runs, live database:

| Statement | Before | After |
|---|---|---|
| /events, rows (page 1) | 179 ms | 0.16 ms |
| /events, rows (page 100) | 366 ms | 4.8 ms |
| /events, total | 110 ms | 1.1 ms |
| /events/security, rows (page 1) | 165 ms | 0.12 ms |
| /events/security, total | 21 ms | 20 ms |
| `ng_overview.security_events` | 20 ms | 16 ms |
| /events/device, busiest device, rows | 197 ms | 2.4 ms |
| /events/device, busiest device, total | 129 ms | 1.4 ms |
| `SELECT_DEVICE_EVENTS` (7 days) | 123 ms | 0.36 ms |
| `SELECT_SECURITY_EVENTS` (1 day) | 157 ms | 0.05 ms |
| `SELECT_SECURITY_EVENT_COUNT` (1 day) | 160 ms | 0.16 ms |

Not fixed, because they need an index only the daemon can add:

- /activity and /events/new-devices filter on `new_device` (and the person
  types), which no daemon index covers. Sorting them on `"timestamp"` made
  the planner walk the time index looking for rare rows, slower than the scan
  (Activity 250 ms against 160 ms live), so they keep their sort. A daemon
  index `ON ng_events (event_type, "timestamp" DESC) WHERE event_type <>
  'ip_changed'` takes both under 0.2 ms with no change to the plugin.
- The Alerts page's "Recently sent" (`SELECT_RECENTLY_SENT`, `WHERE notified`)
  has no index to read. Walking the time index backwards until 20 sent alerts
  are found reads the whole log when fewer exist, which is the live case (no
  event is marked notified): 297 ms against the 93 ms scan. A daemon index
  `ON ng_events ("timestamp" DESC) WHERE notified` answers the current
  statement in 0.01 ms.

Tests plan the kernel's own SQL for the three gathers (built with its
`GatherQueryBuilder` from the shipped gathers and record types) and the three
assistant statements against the daemon's indexes, and fail on the old ones.

**The two requests the site serves every minute no longer scan the whole
event log, so they finish under the 100 ms slow threshold instead of logging a
WARN or ERROR each time.** A 78-hour field run logged about 120 slow-request
lines an hour (2,922 of them ERROR). Matched by timestamp against the cron
log and the status checker's log, they were almost all two requests, one each
a minute: the cron POST (about 450 ms) and the status check's GET of
`/overview` (about 290 ms). In both the time went into sequential scans of
`ng_events`, which on that site holds 4.1 million rows (1.1 GB, mostly
`ip_changed`). The database had done 406,000 sequential scans of it and read
530 billion rows. The database pool was never short of connections. Its
"100% (2/2)" warnings count only the connections open at that moment, out of
a maximum of 10.

- *Cron: the retention prune.* `DELETE_EXPIRED_EVENTS` compared the cutoff
  against `timestamp_epoch`, the daemon's generated epoch column, which has no
  index. It runs on every tick, and nearly every tick has nothing to delete,
  so each minute a full scan of the table found that out. It now compares
  `"timestamp" < to_timestamp($1)`, which the daemon's `ng_events_time_idx`
  answers. On the live table an empty prune went from 396 ms to 0.015 ms.
  On a synthetic copy with the live row counts it went from 724 ms to
  0.05 ms, and a prune that does delete 1,000 rows takes the same 1.7 ms as
  before.
- *`/overview`: arrivals and departures.* `ng_person_movements` filters on
  `event_type` and on `day`, and `day` is `to_char("timestamp", …)`. No index
  serves either, so the overview's `movements_today` count and its movements
  include each scanned the whole log, about 140 ms apiece. Migration 019
  adds `e.person_item_id IS NOT NULL` to the view where `ng_events` has
  netgraspd V5's `person_item_id` column. The daemon sets that column on
  every person event and indexes it (`ng_events_person_idx`), so the view now
  reads only the person events. Its columns and rows are unchanged: on the
  synthetic copy (4.12 million events, 3,000 of them person events) the full
  view hashes the same before and after. The include went from 88 ms to
  1.8 ms and the overview view from 136 ms to 13 ms. On the live database the
  include went from 145 ms to 0.08 ms and the overview view from about
  210 ms to about 20 ms. With no V5 column (an older daemon, or tables the
  plugin created itself) 019 does nothing. Running it again picks up a daemon
  upgraded later.

**The network assistant counts from the database, so "how many are offline"
and "which vendor has the most" match SQL on any size of network (#33).**
`list_devices` was capped at `assist::RESULT_MAX_BYTES` (12,000 bytes), about
100 device lines, but headed its result with the full count, so on a
171-device network the model counted the first 109 rows as all of them: 79
offline where there were 78, Ubiquiti 40 where there were 41. Its lines had no
vendor, so vendor counts came from MAC prefixes, and it had no filter for idle
or offline. It now opens with counts by state, vendor, type and owner, computed
in SQL over every matching device and never cut; takes `offline`, `idle`,
`vendor:<name>` and `type:<name>` beside the old filters, and a `limit` (default
40, 0 for counts only); puts the vendor on every line; and, when rows are cut,
says "showing N of M" and how to narrow in both the result and the chat line.

**Relative times round, and `new_devices` takes a window, so "joined in the
last 24 hours" is answered right (#32).** `assist::relative` truncated: a device
first seen 37 hours earlier read "1 day ago" and was reported as new in the
last day, and 1 hour 56 minutes read "1 hour ago". It now rounds to the nearest
unit and keeps the hours past a day ("1 day 13 hours ago", "about 2 hours
ago"). `new_devices` takes `hours` (default 168, as before) and states its
window with its start in UTC.

**The assistant can say what happened.** No tool read the event log, so "how
many gratuitous_arp events yesterday" or "what is scanning" had no answer. A
new read-only `events` tool takes a window (`hours`, default 24, or `from` and
`to`, at most seven days) and filters by type, security class, device and
"sent as an alert", and returns counts by type and by device over every
matching event before the newest rows.

**A three-question network conversation fits the 60k token cap.** The network
scope's context listed every person and every device grouped by owner on every
model call: on a 171-device network about 9,800 bytes, roughly 4,700 of the
6,700 tokens each call started with, so two of three field-run conversations
went over the cap. The context now carries the clock, when the data starts, the
device totals by state and owner, and the people (about 900 bytes on the same
network), and the scope's prompt says which tool answers the rest. Replaying
the field run's conversations against the new tools measures 40k to 48k tokens
each, from 59k to 70k.


**Two people whose names differ only in case or surrounding spaces can no
longer both exist, even when the two creates race.** The rule that "jamie " is
Jamie (`assist::same_name`) was enforced only in the plugin:
`find_or_create_person` read every person, found nobody by that name, and then
created one. Two creates arriving together could both pass the read before
either had written its `ng_people` mirror row, so both people were made, and
nothing underneath said otherwise: `ng_people` had no uniqueness on the name.
Migration 018 adds a unique index, `ng_people_name_key`, over
`lower(btrim(name))`, the SQL spelling of the same rule. When the mirror write of
a new person is refused, `find_or_create_person` takes back the Item it just
created through the forms' existing undo and returns the person who won, so
both callers are given the same person. A test holds the table locked until
both creates are past the plugin's check, then releases it, so the race is
exercised rather than hoped for.

018 builds nothing over people who already share a name: it fails and names
each of them (name and Item id), and changes nothing, so somebody can merge or
rename them and run it again. Like 015's index it is guarded on ownership: a
kernel role that does not own `ng_people` skips it, and the README's role
section now has the statement for the table's owner to run.

## [1.1.0] - 2026-11-18

The web interface: a site menu of Home, Devices, People, Alerts and Activity,
a Home that says who is home and what needs attention, one Devices
page, People you can create, merge and delete, and an Alerts page with a Send
test. Device data now needs a sign-in (see Changed). Every old path still
answers.

| | |
|---|---|
| Pairs with | netgraspd 1.1.0, `ng_` schema version 5 (reads version 4's notify tables, none of version 5's yet) |
| Built against | Trovato 0.102.0, `trovato-sdk` revision `ca76a96`, `KERNEL_API_VERSION` (0, 102) |
| Manifest | `version = "1.1.0"`, `api_version = "0.102"` |
| Runs on | Trovato 0.102.0 or a later 0.x; tested on the `0.102.0` image, and installed on `0.104.0` |

Upgrade the daemon to 1.1.0 and run its `migrate` first. An older daemon writes
no delivery status and never answers a Send test, so the Alerts page has
nothing to show for either.

### Added

**One Alerts page, `/alerts`, in the site menu between People and Activity.**
Before, there was no answer anywhere to "will I be told when Jamie gets home,
and will it reach me": the two notify flags sat on each person's and device's
forms, and the plugin could only guess delivery from `ng_events.notified`
because the destination lives in the daemon's config. The page now says every
rule as a sentence ("Tell me when Jamie gets home."), grouped by person and
then by device, each with a switch; where alerts go and whether the last one
was delivered; and the last 20 alerts sent, in the daemon's own words.
`/alerts/add` sets a rule for a person or a device and `/alerts/devices` sets
exactly which devices alert; both write through the existing notify saves and
nothing else. **Send test** inserts one row into `ng_notify_requests`, which
netgraspd answers on its next reconcile tick, and the confirmation page shows
the outcome. The plugin still sends nothing and reads no secret: the daemon's
status row names the transport (`ntfy`), never the topic, server or token.

The two tables are netgraspd's schema V4 ([netgraspd#23](https://github.com/jeremyandrews/netgraspd/pull/23)). `015_netgrasp_notify_schema.sql` is a
guarded copy, as `001` is for V1 to V3, and `016_netgrasp_alerts.sql` adds the
menu row. Upgrade and migrate the daemon first.

Tests: `the_alerts_page_says_every_rule_where_alerts_go_and_what_was_sent`,
`an_alert_added_for_a_person_turns_on_that_alert_alone_on_both_tiers`,
`an_alert_added_for_a_device_turns_its_notify_on_and_nothing_else`,
`the_bulk_alerts_form_sets_exactly_the_listed_devices`,
`send_test_inserts_one_request_for_the_daemon_and_nothing_else`,
`the_alerts_page_switch_turns_one_person_rule_off_and_keeps_the_other`,
`the_alerts_pages_refuse_a_caller_without_the_permission_and_anonymous`
(host-in-the-loop); `the_alerts_page_reads_the_daemons_notify_status`,
`a_test_request_is_inserted_and_its_outcome_read_back`,
`recently_sent_lists_only_delivered_events_in_the_daemons_words`,
`the_alerting_devices_are_counted_and_listed_by_owner` (against the daemon's
DDL); and the faithful-copy check now covers the notify tables.

**Give a device an owner who does not exist yet.** The owner form lists
everybody by name, with a search box when scripting is on, and under it "or a
new person named". A name nobody has creates that person and assigns the
device in one POST; a name that differs from an existing person's only in case
or surrounding spaces is that person ("jamie " is Jamie), and the page says
"the existing Jamie". The same rule, `assist::same_name`, now decides it for the
assistant's `create_person` too, which also refuses a duplicate at Execute and
not only at Describe. Before this, the form's only choice on a fresh install
was "nobody".

**Add person, and sort several devices at once.** `/people/add` (a name, and
"Alert me when they get home"), linked from People and from Home's empty state.
`/devices/assign` lists every device with no name and no owner, online first,
one checkbox each, with one owner field; above the list, one line per owner a
possessive mDNS name announces ("2 devices are named 'Jeremy's ...': select
them"), which ticks those rows and changes nothing until Assign is pressed.

**Merge two people who are the same, and delete one.** "Merge into..." and
"Delete..." on every person, each a confirmation page that lists what will
change before anything does. A merge moves every device of the merged person to
the kept one on both tiers, keeps the kept person's alert settings (and the
merged person's alerts too when the pre-ticked box is left ticked, so a merge
never silently turns an alert off), and removes the merged person. Their past
comings and goings are attributed to the kept person through a plugin-owned
`ng_person_merge` table that `ng_person_movements` now joins
(`014_netgrasp_people.sql`); the daemon's `ng_events` rows are not rewritten. A
delete unassigns the person's devices, never deletes them.

**A person page.** `/item/<uuid>` for a person now says whether they are home
and since when, lists their devices and their last 20 comings and goings, says
their alerts in words, and links Rename, Alerts, Merge into..., Delete... and Add
a device. People and Home now have tabs: Everyone, Home now, Comings and goings.

### Fixed

**The plugin installs when the kernel's role does not own the daemon's tables.**
`015` created `ng_notify_requests_pending_idx` with `CREATE INDEX IF NOT
EXISTS`, but Postgres checks ownership of the table before it looks at the index
name, so a kernel connecting as a role of its own (the daemon migrated first, as
documented) failed `plugin install` with `must be owner of table
ng_notify_requests`, and a deployment that runs `plugin install && serve` never
started. The index is now guarded on ownership, as `001`'s are. A test applies
`001`, `015` and `017` as a role that owns no daemon table, and the same failure
was reproduced and cleared against the published kernel image.

**No second copy of the daemon's open-row indexes (#15).** Where the kernel
connected as the role that owns the `ng_` tables, `001` added
`uniq_ng_presence_open` and `uniq_ng_location_open` beside the daemon's own
identical `ng_presence_open_idx` and `ng_location_history_open_idx`, so every
presence and location write maintained two indexes, and the daemon's preflight
never saw the extras. `017` drops one of the plugin's two names only where
another valid unique index on the same table covers exactly `device_id` with
the same predicate, so a daemon index is never touched, and an install with no
daemon keeps its own as the only guarantee of one open row per device. The test
fixture now gives the daemon's indexes the daemon's names, which is how the
duplicate went unseen.

**Stale kernel versions (#14).** The demo compose file said the manifest
declares `api_version 0.99`; it declares `0.102`, and the comment now says where
both come from. The README's count of the published image's plugins is the one
`0.102.0` has.

**Installing from the Release tarball onto the published image (#16).** The
README said the demo compose file shows how, but it builds from source, mounts
the clone and brings its own database. It now has the `docker run` lines for
the published image with the tarball and the daemon's database, which database
role to use and the grants a separate one needs (including `INSERT` on
`ng_notify_requests` for Send test), and why the `WASM not found` warning is
harmless.

**A person write that fails part way leaves nothing behind.** Creating,
assigning in bulk, merging and deleting each touch two stores the plugin cannot
hold in one transaction (the kernel's `item` table through `item-api`, and the
daemon-facing rows through `db`, which commits each call on its own). So the
rows that must change together are one statement, the Item delete that cannot
be undone is always last, and a failure undoes the steps before it through the
same write functions. Tested by making the database refuse a step part way
through each one.

**Four entries in the site menu instead of fifteen.** Home, Devices, People and
Activity, written as `menu_link` rows by `011_netgrasp_navigation.sql`. The
kernel draws `menu_link` rows in place of every plugin's `tap_menu` entries
whenever there are any, so the kernel's own Home, Translate, Post, Blog, Terms,
Translate Language and Media left the bar along with netgrasp's eight flat
entries; at 1440 px the old row was 1593 px wide. Netgrasp's `tap_menu` now says
the same four things, as the fallback for a site with no `menu_link` rows. The
kernel draws no second menu level, so a section's other pages are tabs the page
draws itself. `/devices` opens on what is online, the full list moved to
`/devices/all`, and `/who-is-home` is a 301 to `/people/home`; every other path
answers as it did.

**One Devices page, with tabs and a search box.** Online (`/devices`), All,
New, To sort, Hidden (new, `/devices/hidden`), By place and By type, which now
draws one section per type. Every tab takes the same search over every name,
the MAC, the vendor, the IP and the owner: an exposed `contains` filter on a
`search_text` column, which a blank box drops, so it matches everything. The
column lives on two new views over the old ones (`012`), because
`CREATE OR REPLACE VIEW` cannot drop a column and 006 and 007 must stay
re-runnable. Below 640 px every table is a stack of cards with its row menu, so
no page scrolls sideways on a phone.

**Activity** (`/activity`): arrivals, departures, new devices, and the three
security events a person can act on, newest first, with the rest of the event
pages as its tabs. The whole log is 95% `ip_changed` on a real network, which is
why it is not the default.

**Home answers who is home and whether anything needs you.** `/overview`, the
menu's first entry, is now one line per person at home with since when and
where, the people who are away in one muted line, today's comings and goings,
and *Needs attention*: one line each for ARP spoofing, a rogue DHCP server or an
address conflict in the last day, up to six devices new this week that nobody
has named or assigned (each with an Assign link to the owner form), and "N more
devices have no owner. Sort them". Then how many devices are online. The raw
security total is gone: on the live network it was 169,785, 98% scan noise. With
nobody added yet the page says so in one sentence and links to adding a person.
The counts are a view over the old one, `ng_home` (`013`), for the same
re-runnable-migration reason as `012`.

**The plugin as a container image.** `ghcr.io/jeremyandrews/netgrasp-trovato`,
multi-arch (arm64 and amd64), published on every push to `main` as
`sha-<12 character commit>` and on every `v*` tag as the version and `latest`.
Until now the only artifact was the release tarball, which a cluster cannot pull:
a Kubernetes deployment had no way to get the plugin without a checkout or a
download step of its own. The image is the tarball's three directories under
`/netgrasp`, built by a new `published` stage in `docker/overlay.Dockerfile` that
runs `scripts/build-overlay.sh` like every other consumer, so the capability check
still runs. That Dockerfile's build stage now runs on the builder's native
platform, since the wasm is identical on every architecture and compiling it
under emulation would buy nothing but time.

**An overview, and it is the front page.** `/overview`, from
`007_netgrasp_overview_views.sql` and `008_netgrasp_overview.sql`. Who is home
and since when, today's arrivals and departures in order, the devices first seen
this week with the daemon's fingerprint and how sure it is (each with the row
menu, because a new device is the one you want to name and assign), and the
security event count linking to `/events/security`. `/` redirected to
`/devices/online` before; it redirects here now, but only where that was still
the setting, so an operator's own front page is untouched.

What was missing underneath was not a template but a way to put four questions
on one server-rendered page. A gather renders one query, its template cannot run
another, and a `gather_query` tile is an empty element for client script to fill.
The kernel's gather `includes` does compose, so the overview is a gather over a
one-row view whose columns are the counts, with the three lists attached as
includes. Three of the questions are relative to the clock ("today", "this
week", "the last 24 hours"), and a gather filter has no clock offset and resolves
`current_date` in the kernel's time zone rather than the database's, so they are
views that call `now()` in the same session that compares the rows. DESIGN.md
Decision 11.

Each list is a page of its own too: `/people/home` (people the daemon counts as
home, with their arrival time, which `ng_people` stores as a `timestamptz` with
no epoch twin, so the view computes one), `/people/movements` (the arrival and
departure log, or one day with `?day=`), and `/devices/new`. Every page reloads
itself on the same interval as the others.

**`scripts/verify-demo.sh`, and a `verify-demo` CI job that runs it.** It brings
up `docker-compose.demo.yml` as the README describes, then checks that `/`
redirects to `/overview` and that the page is the overview's own template: a
gather template that raises answers 200 with the kernel's column dump, so a
status code alone would pass a broken page.

### Changed

**Device data needs a sign-in.** Before, `curl /devices/online` with no session
returned every MAC address on the network: a gather has no permission of its
own, and the anonymous role held `view netgrasp devices`. The plugin now
implements `tap_field_access`, denying every netgrasp field to a viewer without
that permission, and `013` revokes it from the anonymous role. A visitor sees
the pages empty, with a line asking them to sign in. The README gives the one
statement that grants it back for a wall screen.

### Tests

Host-in-the-loop, against the real `GatherService` and a real Postgres:
`the_overview_counts_and_lists_what_the_house_is_doing_today`,
`the_overviews_listings_are_pages_that_filter_and_render_on_their_own`,
`an_empty_house_still_has_an_overview`,
`every_overview_view_carries_every_column_its_record_type_maps`,
`no_overview_include_is_named_after_a_column_it_would_overwrite`,
`each_overview_list_agrees_with_the_page_it_links_to`,
`the_front_page_moves_to_the_overview_only_from_the_old_default`. Unit:
`the_overview_and_its_listings_render_with_the_rows_they_will_really_get`,
`the_overview_counts_exactly_the_declared_security_event_types`,
`the_overview_becomes_the_front_page_only_where_the_old_default_stands`.

### Added (location)

**Where a device is, on its row and on its page, and a page by place.** The
device tables grow a Where column: the place the daemon's UniFi enrichment
resolved, linked to its section of the new `/devices/location`, with the access
point under it. The device page adds an Access point row beside Location.
`/devices/location` (`009_netgrasp_locations.sql`) groups every placed device
under its place.

What had to be decided underneath was what these read like with enrichment off,
which is the default and most installs: `current_location` and `current_ap` are
null on every row. So the Where column is rendered only when some row on the
page has either value (counted with Tera's `map`, which drops nulls), the device
page omits both rows instead of printing empty ones, its location timeline says
the location would come from the daemon's enrichment instead of "No location
history", which read as a gap in monitoring, and `/devices/location` is its
empty state saying the same. The device page read `current_location` before and
never `current_ap`; `SELECT_DEVICE_STATE` now reads both.

Tests: `the_location_page_groups_every_placed_device_under_its_place`,
`with_enrichment_off_the_device_pages_read_cleanly_and_say_why_location_is_empty`,
`the_device_page_shows_the_access_point_and_explains_a_missing_location`
(host-in-the-loop; all three fail with the templates, manifest and query as they
were), `the_where_column_appears_only_when_something_on_the_page_is_somewhere`,
`the_identity_block_names_the_place_and_the_access_point`,
`with_no_enrichment_the_page_omits_location_and_says_where_it_would_come_from`,
and the daemon-schema test now decodes `current_ap` null and set.

### Added (new devices)

**A to-do for new devices, and the record of them.** `/devices/todo` lists every
device nobody has named or given an owner, newest first, with the row menu that
does both; it is in the navigation second, after the overview, and the overview's
new-device section links to it. `/events/new-devices` is the daemon's
`new_device` events on their own. `010_netgrasp_new_devices.sql`.

The to-do is over devices as they are now, not over the event log: a device
whose event was pruned at 90 days and never named is as unfinished as one that
appeared an hour ago. "Unnamed" means no name a person typed, so a device the
daemon guessed a name for stays on the list with the guess shown, and confirming
it is one rename. A device needs both missing to be listed, because a named but
unassigned device is usually infrastructure that belongs to nobody, and a list
that never empties is one nobody reads.

Tests: `the_todo_lists_unnamed_unowned_devices_until_somebody_names_or_assigns_them`
(renames one through the row menu's real form and watches it leave),
`the_new_device_event_page_lists_only_new_device_events` (host-in-the-loop; both
fail without 010), `the_todo_and_the_new_device_events_render_and_point_at_each_other`.

### Added (assistant)

**"Who came home today?" and "What is new this week?" in conversation.** The
network scope gains two read tools. `arrivals_and_departures` gives one day's
arrivals and departures in order (today by default, or `day: YYYY-MM-DD`), with
where and through which access point, then who is home now and since when.
`new_devices` gives the devices first seen in the last seven days, with the
fingerprint and its confidence, the owner, the daemon's name guess, and how
many still need a person to name or assign them. No write tools were added.

They read the overview's own views through three new statements in
`netgrasp_core::queries`, and "today" is asked of the database rather than
worked out from the plugin's clock, so the conversation and `/overview` cannot
disagree about which day it is or what counts as new. A day that is not a
calendar day ("2026-02-30") is refused rather than answered with an empty day.

Tests: `the_network_assistant_answers_who_came_home_today_and_what_is_new` and
the extended `the_three_scopes_are_declared_and_the_kernel_registry_accepts_them`
(host-in-the-loop; both fail against the module built from the previous
`assist_host.rs`), `the_overview_reads_refuse_a_caller_without_the_permission`,
`the_overview_reads_decode_through_the_db_host_over_the_daemons_schema`, and six
unit tests over the day parser and the two renderers.

## [1.0.0] - 2026-09-23

The first release. Everything from the extraction out of the Trovato monorepo
to the row menu, listed below this entry by pull request; this entry itself is
what landed after the first joint run with the daemon.

| | |
|---|---|
| Pairs with | netgraspd 1.0.0, `ng_` schema version 3 |
| Built against | Trovato 0.102.0, `trovato-sdk` revision `ca76a96`, `KERNEL_API_VERSION` (0, 102) |
| Manifest | `version = "1.0.0"`, `api_version = "0.102"` |
| Runs on | Trovato 0.102.0 or a later 0.x; tested on the `0.102.0` image |

The daemon it pairs with has never contacted a real UniFi controller; its own
release notes say so first. Nothing on this side depends on the controller.

### Added

**Releases.** `.github/workflows/release.yml`. A `v*` tag builds the module,
assembles the overlay with `scripts/build-overlay.sh` (which also checks the
manifest's capabilities against the module's imports), and attaches
`netgrasp-trovato-<version>.tar.gz` and its `.sha256` to the GitHub Release. The
tarball is the three directories a deployment appends to Trovato's search
paths, `plugins/`, `templates/` and `static/`, so installing needs no Rust. A tag
that disagrees with the workspace version or the manifest, or that has no
section in this file, is refused before anything is built.

**Every listing row has an actions menu, and it works with scripting off.**
`templates/gather/netgrasp/row-actions.html`, `static/js/netgrasp.js`,
`static/css/netgrasp.css`.

Device rows, event rows and person cards carry a `⋯` menu: rename, assign an
owner, hide or unhide, mute or unmute alerts, and open a conversation about the
row. It is a `<details>` element with a list of links in it, so the browser opens
it on click and on Enter or Space and every entry is reachable with no
JavaScript at all. Right-click opens the same element under the pointer and
closes any other; Escape closes it and puts focus back on the button. Touch gets
the visible button, which is why there is a visible button.

**No entry writes from the row, and that is the kernel's decision rather than
this plugin's.** A gather content template is rendered in its own Tera context —
twelve keys, all of them about the query — and the site context carrying
`csrf_token` is built afterwards for the page around it
(`crates/kernel/src/routes/gather.rs`, `render_gather_with_theme`). A
`<form method="post">` in a row would post with no token, and the kernel refuses
a state-changing plugin request without one *before* dispatch, so the form would
403 and the plugin would never be called. Each entry therefore links to a page
`tap_api` serves, which is handed a token and renders the form.

The same limitation is why "Assign owner" is a link rather than an inline
select: a gather reads one record type, so the people list is not in a device
page's context either. And it is why the kernel's own assistant launcher has
never rendered on any of these nine pages — `assistant_enabled` is in the context
that does not reach them, and an assistant that is switched off is supposed to
render nothing, so the failure and the intended behaviour look identical.
`FRICTION.md`: `G-GATHER-TEMPLATE-NO-CSRF`,
`G-ASSISTANT-LAUNCHER-NEVER-RENDERS-ON-A-GATHER`, `G-ASSISTANT-NO-SEED`,
`G-NO-ROW-ACTIONS-PARTIAL`, `G-THEME-NO-DARK-TOKENS`.

**The six forms the menu opens.** `plugins/netgrasp/src/forms.rs`,
`plugins/netgrasp/netgrasp.info.toml`.

`tap_api` serves `/netgrasp/device/{rename,owner,hidden,notify}` and
`/netgrasp/person/{rename,notify}`, each as a `GET` that renders one field with
the kernel's `_token` in it and a `POST` that writes. Every route is invisible in
navigation, gated on `administer netgrasp`, and checked again inside the plugin
at the moment of the change — the same two checks the assistant's tools make, and
they still disagree about `administer site` for the reason
`G-USER-API-NO-ADMIN-BYPASS` gives.

A device is named by its MAC **or** by its `ng_devices` row id, parsed by the
function the assistant's tools parse theirs with, which is what lets an event row
— whose only handle on a device is `device_id` — reach the same forms a device
row does.

**The writes are the assistant's own, not a second copy of them.**
`apply_device_edit` and `apply_person_save` are what the tools call, so a device
renamed from a menu and one renamed in a conversation mint the Item identically,
coerce the MAC identically, write the same columns and leave `sync_state` alone
identically. The edit stays **sparse**: a rename names `display_name` and nothing
else, which is the discipline that exists because a rename once turned a device's
alerts off. A form posting a whole overlay would have brought that back, and the
host-in-the-loop tests assert the columns nobody named are untouched rather than
only that the named one is right.

**The auto-reload defers instead of firing while the page is in use.** A menu
standing open or a focused field re-arms the timer rather than reloading.
Reloading ten seconds after somebody opens a menu closes it before it can be
read; cancelling outright would leave a wall display frozen because somebody
walked past.

**The gather templates are now rendered by a test, not grepped by one.**
`plugins/netgrasp/src/lib.rs`. Every template assertion in this repository was a
string search, and a string search cannot tell a working template from one that
raises on the first row — which matters here more than it sounds, because a
gather template that raises falls back to the kernel's dump of every column of
the base table, so the page still renders and nothing looks wrong. The new test
renders all three listings through real Tera with the rows they actually get,
nulls included. It caught three defects in this change before any of them
shipped: a filter inside a parenthesised `set` (a parse error, which takes the
template down whole), `urlencode` raising on the event table's integer
`device_id`, and `~` raising on the null `device_id` of an event whose device is
gone.

### Fixed

**A device edit no longer writes a column the tool call did not name.**
`plugins/netgrasp/src/assist_host.rs`, `crates/netgrasp-core/src/writeback.rs`.

Renaming a device turned its arrival and departure alerts off. So did assigning
it an owner. Observed twice in the first joint run against a live LAN, on two
different devices, with the proposal card saying nothing about it
(`docs/JOINT-RUN.md`, plugin finding 1).

Root cause, in three parts, each of which was individually reasonable:

1. The cron sync minted a device Item carrying only `field_mac`, on the argument
   that every other field is the admin's to fill in (`DESIGN.md` Decision 1). So
   the Item and the device row disagreed about four user-owned columns from the
   moment the Item existed, and nothing read both to notice.
2. `writeback::field_bool` reads an absent field as `false` — correct for a
   checkbox that was not ticked, and a fabricated value for a field that is not
   there at all.
3. An assistant tool call built a **whole** `DeviceOverlay` and filled the
   columns it had not been asked about from that Item. `ng_devices.notify` is
   `NOT NULL DEFAULT TRUE`, so the fabricated `false` was written over a `true`
   nobody had changed. A muted device then stayed muted forever, because the
   next edit re-read what the last one wrote.

Fixed at all three:

- `model::DeviceEdit` is a **sparse** overlay: per column, either "set it to
  this" or nothing at all. `writeback::build_partial_update` builds its `SET`
  list from the intersection of `columns::USER_OWNED` and the edit, so a column
  nobody named is not in the statement and has no value to be wrong about.
  `build_update` — the admin content form's path, where the saved Item really is
  the whole new state — is now that same function over a full edit, so there is
  one `SET` builder rather than two to drift apart.
- The sync's mint carries the row's user-owned values onto the new Item, so the
  two tiers agree from birth. This also closes the same defect on the **admin
  form** path, which the partial write-back does not reach: opening a
  field-less Item's edit form and saving it would have written the same
  fabricated `false`.
- `writeback::merged_device_fields` fills a field the edit does not name from
  the Item when the key is present — present, not non-blank, so notes an admin
  cleared stay cleared — and from the device row when it is not.

Audited every other write tool for the same pattern. `rename`, `set_owner`,
`assign_device`, `set_notes`, `set_flags`, `rename_device`, `set_device_flags`
all took it and all now name their own columns. The person tools
(`create_person`, `rename`, `set_notes`, `set_notify`, `set_person_notify`,
`delete_person`) do not have it and are unchanged: `ng_people` is a mirror
derived from the person Item alone, with no second writer to lose a value to, so
a field read back as `false` is the Item's own state rather than a fabrication.
That is a different shape, not a smaller version of the same bug, and it is
recorded here so the next audit does not have to re-derive it.

**A proposal card names the columns the write will change.** Each write tool
builds its `DeviceEdit` before the Describe branch, and the card's change set is
`DeviceEdit::columns` — the same list the statement's `SET` clause is built
from. The displayed change set and the executed change set are now one value
read twice rather than two descriptions that can disagree.

**An Item's title prefers the name a human would use.**
`crates/netgrasp-core/src/sync.rs`, `queries.rs`.

A Brother printer's Item was titled "CLOUD NETWORK TECHNOLOGY SINGAPORE PTE.
LTD. device" while the assistant's card for the same row said "Brother
HL-L8360CDW series". The better name was already in the database and nothing was
reading it (`docs/JOINT-RUN.md`, plugin finding 2).

Root cause: two naming ladders over two row shapes. The sync derived a title
from a `DeviceRow`, whose projection read `hostname` and `vendor` and not
`resolved_name` or `mdns_name`; the assistant labelled a `DeviceFacts`, whose
projection read all four. The daemon's `resolved_name` is the answer its identity
resolution settled on, so skipping it meant preferring the OUI holder's
corporate name over the device's own.

There is now one ladder, `sync::observed_name`, and one title function,
`sync::device_title`, used by the sync's mint, the sync's title refresh and the
assistant's context alike: `display_name`, then `resolved_name`, then
`hostname`, then `mdns_name`, then the vendor qualified as a device, then the
MAC. `SELECT_DIRTY_DEVICES` and `SELECT_DAEMON_TITLE_FIELDS` read the columns
that ladder needs.

Existing Items re-title themselves on the next sync pass, through the
`SyncAction::Refresh` branch that already existed: the derived title moves, the
pass notices, and the Item follows. **A hand-set name is not overtaken**, and
the plugin does not have to guess whether a title was edited by hand: the
write-back stores a typed title in `display_name`, and `display_name` is the
first rung of the ladder, so a device somebody has named keeps its name however
much the daemon learns afterwards. The one residual case is a device whose
`display_name` was pinned to the old vendor-derived form by an earlier
write-back; those keep the stale title until somebody renames them, and no
migration guesses at them, because the column is indistinguishable from a name a
human typed.

**The assistant is told when monitoring began.**
`crates/netgrasp-core/src/assist.rs`, `queries.rs`.

Asked who was online yesterday against a database an hour old, the model
correctly found zero presence spans and then speculated about "a gap in
monitoring": an empty window before the first observation and an empty window
over a monitored period are the same zero rows, and nothing in its context told
them apart (`docs/JOINT-RUN.md`, plugin finding 3).

The network scope's context now opens with the earliest observation the database
holds — the older of the first `ng_presence` session and the first `ng_events`
row, via `SELECT_MONITORING_START` — and the current time, stated plainly. The
device scope says the same about that device's own `first_seen`. A database with
nothing in it says so rather than implying a monitored silence.

The statement aggregates the `timestamptz` and extracts the epoch from the one
resulting value rather than aggregating the generated `_epoch` twin: both give
the same number, and only that form can use the daemon's indexes, which the
generated columns do not have.

### Changed

- **The plugin's version is its own.** The workspace and the manifest move from
  0.102.0, which was the kernel's version, to 1.0.0. The Trovato version now
  lives in `[workspace.metadata.trovato]` beside the revision, and
  `the_manifest_declares_the_pinned_kernels_api_version` reads it from there
  instead of from `CARGO_PKG_VERSION`. It also checks that the recorded revision
  is the one both Trovato dependencies pin, and that the manifest's `version`
  is the workspace's. The kernel stores and displays a plugin's `version` and
  compares nothing against it, so this changes no behaviour.
- `model::DeviceRow` carries `resolved_name`, `mdns_name` and the rest of the
  user-owned set (`notes`, `hidden`, `notify`, `owner_item_id`). The two flags
  are `Option<bool>`, so "this projection did not read it" stays distinguishable
  from `false` — which is the distinction whose absence caused the first defect
  above.
- `sync::daemon_title` takes a `sync::TitleInputs` rather than a `DeviceRow`, so
  the write-back's naming probe no longer builds a fake row with `id = 0` to ask
  it a question.
- `assist::render_network_snapshot` takes the earliest observation.

### Documented

- `plugins/netgrasp/FRICTION.md` gains the seven **kernel** defects from the same
  joint run (the assistant's unconditional `temperature`, the discarded provider
  error body, the permissions page deleting every plugin grant, no way to put a
  user in a role, the single-use CSRF token on the chat page, the zeroed usage in
  the stream's `done` event, and the provider test that calls a 404 a success).
  Reported, not patched: nothing in this repository changes the kernel.

## Before 1.0.0

No release was tagged, and the version tracked the Trovato kernel the plugin was
pinned to. Listed by pull request, oldest first.

**In the Trovato monorepo, 2026-08-01 to 2026-08-15.** The design gate (a device
is two tiers, an Item for what a person edits and a record for what the daemon
writes; an event is a record), the daemon bridge (sync, write-back, timelines),
and the first friction log. On 2026-08-06 the plugin was reconciled with the
daemon's real schema, which it had been written without: it had declared UUID
keys, epoch integers and four column names the daemon does not use, so against a
real daemon database every timeline query failed. On 2026-08-15 it was made to
work against a live daemon database and given usable pages.

**#1, 2026-08-18: its own repository.** Extracted with `git filter-repo`,
keeping each file's history. `trovato-sdk` became a git dependency on the public
Trovato repository, **pinned to revision `611c1fb` (Trovato 0.99.0)**, with
`api_version = "0.99"`; the web interface was finished with no Trovato patch.
Version 0.99.0.

**#3 and #4, 2026-08-18.** The host-in-the-loop integration test moved here from
Trovato, driving the real module through the real kernel against Postgres, with
`trovato-kernel` as a dev-dependency on the same revision. The
`ng_devices_with_owner` view, so a device page names its owner instead of
printing a uuid.

**#5, 2026-08-21: the one-command demo.** `docker-compose.demo.yml` on the
published `ghcr.io/jeremyandrews/trovato:0.101.0` image, with the overlay
assembled in a container, so a stranger needs only Docker. A `0.99` manifest
loads on a `0.101` kernel, and a test pins that pairing.

**#6, 2026-08-24: the pin moves to Trovato 0.102.0.** `trovato-sdk` and
`trovato-kernel` to revision `ca76a96`, the workspace and manifest to 0.102.0
and `api_version = "0.102"`, and the demo image to `0.102.0`, together. This is
the pin 1.0.0 ships with.

**#7, 2026-08-25: configuration by conversation.** Three assistant scopes, one
device, one person and the whole network, using the assistant taps Trovato 0.102
added, with write tools that check the permission at the moment of the change.

**#8, 2026-09-18: the first joint run.** `docs/JOINT-RUN.md`: the daemon and this
plugin against one database on a real home network, with the assistant on a
real model. Its findings went to the kernel, this repository and the daemon;
this repository's are fixed in 1.0.0 above.

**#9, 2026-09-18.** An assistant edit changes only what it was asked to change.
In 1.0.0 above, under Fixed.

**#10, #11 and #12, 2026-09-19: the row menu and its six forms.** #10 landed the
menu. #11 was merged into #10's branch after that branch had been squashed onto
`main`, so its forms never reached `main`; #12 restored them. In 1.0.0 above,
under Added.
