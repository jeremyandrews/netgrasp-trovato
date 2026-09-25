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

### Added

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
