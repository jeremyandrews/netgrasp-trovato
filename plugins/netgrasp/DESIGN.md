# Netgrasp plugin — design gate

The scope for this build (TROVATO-CLOSE A7 / NETGRASP 03) carries three
assumptions about the frozen kernel that turn out not to hold. Each of them was
load-bearing for a data-model choice, so this document records what the kernel
actually does, checked at `KERNEL_API_VERSION (1,0)`, and what follows.

The pattern being followed is the one CLOSE 09/A1/A5 landed for Argus: an
in-repo pure WASM plugin (`plugins/netgrasp`) over a host-agnostic core crate
(`crates/netgrasp-core`), with high-churn data as lightweight records and only
the user-editable surface as Items. Nothing in the kernel, WIT, SDK or kernel
migrations was changed.

---

## Drift from the scope's kernel model

### Drift 1 — there is no per-content-type opt-out of embedding

The scope says: *"kernel auto-embed is now async with per-content-type opt-out —
the old 'events-as-Items costs an embed per event' caveat is DISSOLVED: opt
`ng_event` out of embedding in this plugin's declaration."*

`EmbedPolicy` (`crates/kernel/src/services/embed_index.rs:64-95`) has exactly one
field, `sync_types: Vec<String>`, and `is_async()` returns whether a type is
**absent** from it. A type listed there does not skip embedding — it opts out of
the *async* path and takes the pre-P11f **synchronous** embed on the save path
instead (`crates/kernel/src/content/item_service.rs:653-655`). Both branches call
the provider. There is no third value that means "do not embed this type".

It is also not declarable by a plugin. The policy is read from `site_config`
under `embed_policy` (`EMBED_POLICY_CONFIG_KEY`, `embed_index.rs:62`,
`EmbedPolicy::load` at `:83`); `PluginInfo` has no embedding section
(`crates/kernel/src/plugin/info_parser.rs`). A plugin cannot opt anything out of
anything, in this plugin's declaration or elsewhere.

### Drift 2 — a plugin's `save-item` never embeds anyway, because it bypasses `ItemService`

The `save-item` host function calls the **model** directly — `Item::update` /
`Item::create` (`crates/kernel/src/host/item.rs:131,159`) — not
`ItemService::create`/`update`. Everything `ItemService` does around a save is
therefore skipped on the plugin path, including `index_item`
(`item_service.rs:603`), which is what enqueues the embed job.

So the embed cost of plugin-written Items is zero today, but for the opposite of
the reason the scope gives: not because the type opted out, but because the
plugin write path never reaches the embedder at all. This is the mechanism behind
M2's `G-ITEM-NO-EMBED` ("stories have no embeddings, semantic gather needs a
manual admin backfill"), now identified rather than merely observed.

This is not a licence to make events Items — see Decision 2. It removes one cost,
and leaves the one that actually decides it.

### Drift 3 — `tap_item_update` does not fire on a plugin's own `save-item`

Same cause, and it is the most consequential of the three. `tap_item_update` is
dispatched from exactly one place, `ItemService::update`
(`item_service.rs:553`, plus the revision-revert path at `:1398`). Its callers are
the admin content routes and the JSON item routes
(`state.items().update(...)` — `routes/admin_content.rs:513,664`,
`routes/item.rs:882`). `save-item` does not go through it.

The scope asks us to "guarantee the sync loop terminates". Under the frozen
contract it terminates **by construction**: the plugin's daemon→kernel sync
writes Items through `save-item`, which cannot fire the tap that would trigger
the kernel→daemon write-back. The two directions are disjoint at the contract
level, not by a convention this plugin maintains.

That is a strong guarantee and a fragile one — it holds because of an omission,
not a decision, and the day `save-item` is routed through `ItemService` (which is
the obvious correctness fix for Drift 2) the loop closes. So the plugin **also**
carries the belt-and-braces discipline that would make it terminate anyway
(Decision 4), and a test pins the kernel behaviour so the day it changes, a test
says so.

---

## Decisions

### Decision 1 — a device is a **split**: a daemon-owned record and a user-owned Item

Neither single-tier shape is available.

*All-Item* fails on churn. A device's `state`, `last_ip`, `current_location` and
`last_seen` change every time the daemon sees it — an online/offline flip per
device per few minutes on a live LAN. Every push would be an `Item::update`,
which writes an `item_revision` row (`crates/kernel/src/content/item.rs`), so a
40-device LAN would accumulate revisions at the rate of ARP traffic. Item storage
is the wrong tier for something that changes on a timer.

*All-record* fails on writability. The record admin is list-and-view only
(`crates/kernel/src/routes/admin_record_type.rs:181-183`) and no kernel surface
lets a plugin serve a request (M3 `G-NO-PLUGIN-HTTP`), so a lightweight record has
no edit path at all. Naming a device is the feature; a shape where nobody can
name a device is not a shape.

So:

| Tier | What it holds | Written by |
|---|---|---|
| `ng_devices` table, declared as record type `ng_device_state` | daemon-owned identity + volatile state | the daemon; the plugin writes only the user columns and the link |
| `ng_device` Item | the user's overlay: label, owner, notes, hidden, notify | an admin, through the kernel's content forms; created once by sync, carrying whatever the row's user-owned columns already say |

The two column sets are **fixed and disjoint**, which is what makes "the two
writers never collide" a schema property rather than a promise:

- **daemon-owned:** `mac`, `resolved_name`, `identity_source`,
  `identity_confidence`, `hostname`, `mdns_name`, `vendor`, `device_type`,
  `device_type_confidence`, `os_family`, `state`, `last_ip`, `last_ipv6`,
  `last_interface`, `first_seen_at`, `last_seen_at`, `baseline`, `current_ap`,
  `current_location`, `sync_state`, and the generated `first_seen_at_epoch` /
  `last_seen_at_epoch` twins
- **user-owned:** `display_name`, `owner_item_id`, `notes`, `hidden`, `notify`
- **link-owned (plugin):** `trovato_item_id`

The daemon's schema is **canonical** for every `ng_` table: it is the only writer
at runtime and its migrations are already applied, so where the two disagreed,
this side moved. Two things moved on the daemon's side instead, both because the
kernel leaves no alternative: the Item join columns are `UUID` because the
kernel's `item.id` is, and every `timestamptz` carries a generated
`<column>_epoch` companion because the `db` host cannot decode a `timestamptz` at
all and returns it as `null` (`FRICTION.md`, `G-DB-HOST-TYPE-COVERAGE`). Every
time the plugin reads is read from a twin.

`netgrasp_core::columns::USER_OWNED` names the second set once, the
write-back builds its `UPDATE` from that list and nothing else, and a test asserts
the three sets are disjoint and that the write-back statement mentions no column
outside its own set.

### Decision 2 — events are a **lightweight record**, not an Item

Drift 2 removes the embedding argument in both directions, so the decision rests
on what is left:

- **Revisions.** 300 events/day × 90 days ≈ 27,000 Items, each with at least one
  `item_revision` row, for rows that are never edited and are deleted wholesale
  on a retention timer. `item` is a revisioned, access-controlled, translatable,
  taxonomy-capable store; an event uses none of it.
- **Pruning.** As records, the retention pass is one bounded
  `DELETE FROM ng_events WHERE timestamp_epoch < $1` per cron tick. As Items it is one
  `delete-item` host call per row, each dispatching `tap_item_delete` to every
  plugin, 300 times a day, inside a 150 s background epoch.
- **Query shape.** The event log filters on `event_type`, `device_id` and
  `timestamp`. As record columns those are real Postgres types with real indexes;
  as Item fields they are JSONB text extractions.
- **What is given up.** Comments, revisions, semantic search and per-item access
  control on an event — none of which an event log wants.

M1's finding that "the record tier is solid and is the right home for
high-volume articles" applies unchanged to events, which are higher-volume and
less interesting than articles.

`ng_presence`, `ng_ip_history` and `ng_location_history` are the same argument at
higher volume still, and are additionally not standalone entities — they are the
device's timeline. They stay daemon tables, declared as read-only record types so
an operator can inspect them, and are rendered onto the device page by
`tap_item_view` (Decision 5).

### Decision 3 — a person is an Item, mirrored to `ng_people` for the daemon

People are created and edited by a human and there are tens of them, so the
churn argument that ruled devices out of the Item tier does not apply. `ng_person`
is a plain Item.

The daemon needs to read people and ownership without touching kernel tables (it
is a separate process with its own connection and no business knowing the `item`
schema), so `tap_item_insert` / `tap_item_update` / `tap_item_delete` on
`ng_person` mirror the Item into a flat `ng_people` table keyed by the Item id.
The mirror is derived state, one direction only, and the daemon treats it as
read-only.

Ownership is `ng_devices.owner_item_id` → `ng_people.item_id`, so the daemon
answers "whose device is this" with one join and never reads `item`.

### Decision 4 — the write-back is create-once, and terminates for two independent reasons

**Daemon → kernel** (`tap_cron`): rows with `sync_state = 'dirty'` are read in
bounded pages. For each:

- no `trovato_item_id`, or one naming an Item that no longer exists → create the
  device Item (`save-item` with no id), write the id back, set `sync_state = 'clean'`;
- an existing Item → refresh the derived title only, then set `sync_state = 'clean'`.

The Item's title is
`COALESCE(display_name, resolved_name, hostname, mdns_name, vendor || ' device', mac)`
(`netgrasp_core::sync::device_title`, over the ladder
`netgrasp_core::sync::observed_name`), which is the only thing the daemon side
has to say about an Item whose fields are otherwise all user-owned. This is why
the update branch has real work and is not scaffolding.

It is **one** function, and that is the decision rather than the implementation.
It was two: the sync derived a title from a `DeviceRow` whose projection read
`hostname` and `vendor`, and the assistant labelled a `DeviceFacts` whose
projection read `resolved_name` and `mdns_name` too. So a printer's Item was
titled after the Singapore OUI holder its address block came from while every
page and every proposal card called it "Brother HL-L8360CDW series"
(`docs/JOINT-RUN.md`, plugin finding 2). `resolved_name` is the answer the
daemon's own identity resolution settled on, with `identity_source` saying which
signal produced it; a derivation that skips it is preferring a corporate name to
a device's own.

**Kernel → daemon** (`tap_item_update` on `ng_device`): writes exactly
`USER_OWNED` into `ng_devices WHERE trovato_item_id = $id`, and
`display_name` takes the Item's title. It does **not** write `sync_state`.

One subtlety, found by the integration test rather than by design, and worth
recording because the naive version is silently wrong: **an unchanged title must
clear `display_name`, not store it.** `display_name` outranks every observed
name in `device_title`, so if every save stored the title, an admin who edited
only the *notes* of a device still called `aa:bb:cc:dd:ee:ff` would pin that MAC
as its label forever — the daemon could resolve a hostname the next minute and the
device would never take it. So the write-back compares the title against
`netgrasp_core::sync::daemon_title` (what the daemon's observations alone imply)
and stores `NULL` when they match. A name a human actually typed is stored and
wins; a name a human merely failed to change is not. The comparison uses the same
Rust function the sync uses rather than a `CASE` expression in the write-back's
SQL, so the two derivations cannot drift.

Termination, twice over:

1. *By contract* (Drift 3): the cron sync's `save-item` cannot dispatch
   `tap_item_update`, so the loop has no edge to traverse.
2. *By discipline*, which is what survives if (1) is ever fixed: the write-back
   never sets `sync_state = 'dirty'`, so even a firing tap produces a `clean` row
   that the next sync pass does not select. And the title the sync would then
   derive is `COALESCE(display_name, …)` where `display_name` is what the
   write-back just wrote from the title — a fixed point after one pass.

`netgrasp_core::sync::plan` is the pure function that decides create/relink/refresh
/skip, and it is where both properties are tested without a database.

### Decision 5 — the device page is `tap_item_view`, with single-quoted attributes

The presence and location timelines are the plugin's real UI work and there is
one surface for them. `tap_menu`'s `callback` is dropped on deserialize
(M3 `G-NO-PLUGIN-HTTP`), a plugin cannot ship a Tera template, and
`tap_preprocess_item` feeds a template the plugin does not own. `tap_item_view`'s
return value is appended to the item page's children, so that is where the
fragment goes.

It inherits M3's `G-VIEW-OUTPUT-JSON-ENCODED` verbatim: the `#[plugin_tap]` macro
JSON-serializes the return value and the item route appends it undecoded, so the
fragment uses **single-quoted attributes** and an escaper that emits
`&quot;`/`&#x27;`/`&#x5C;` and never a raw `"` or `\`. Same mitigation, same
reason, and a unit test asserts the fragment is free of both characters.

### Decision 6 — no exposed filters; facets are routes

`G-EXPOSED-FILTER-NO-MATCH-ALL` is worse for Netgrasp than it was for Argus,
because more of Netgrasp's facets are uuid columns on a **record** gather: an
exposed `equals` filter left blank binds `''` against a `uuid` column and the
gather **500s**, which is the default state of the page. So every facet is its
own route with a `{"url_arg": …}` filter whose value is always supplied:

| Route | Gather | Facet |
|---|---|---|
| `/devices` | `ng_device_list` | none |
| `/devices/online` | `ng_device_online` | none (`state = 'online'` fixed) |
| `/devices/type?device_type=…` | `ng_device_by_type` | device type |
| `/devices/owner?owner=…` | `ng_device_by_owner` | owner |
| `/events` | `ng_event_log` | none |
| `/events/security` | `ng_event_security` | none (fixed set) |
| `/events/device?device=…` | `ng_event_by_device` | device |
| `/people` | `ng_person_list` | none |
| `/overview` | `ng_overview` | none (one row; Decision 11) |
| `/people/home` | `ng_people_home` | none (`state = 'home'` fixed) |
| `/people/movements?day=…` | `ng_person_movements` | day, optional: an absent `url_arg` resolves to null and constrains nothing |
| `/devices/new` | `ng_devices_new` | none (the view is the week) |
| `/devices/location` | `ng_devices_by_location` | none (`current_location is_not_null` fixed) |
| `/devices/todo` | `ng_devices_todo` | none (no `display_name`, no owner, not hidden) |
| `/events/new-devices` | `ng_event_new_devices` | none (`event_type = 'new_device'` fixed) |

### Decision 7 — the tiles are gather tiles, because a tile cannot count

`G-NO-GATHER-AGGREGATION` is unchanged: `QueryDefinition` has no aggregate
projection and no tile type computes anything. "Online count" is therefore a
`gather_query` tile over `ng_device_online` whose **pager count** is the figure
and whose rows are what is behind it. Same for who-is-home, recent events and
security alerts. The shape is imposed, not chosen.

### Decision 8 — the plugin's migration is a **copy** of the daemon's schema

The daemon owns these tables at runtime, but the plugin's effective DB allowlist
is `migration-owned ∪ db_tables` (`crates/kernel/src/plugin/db_policy.rs`), and a
record type is only admitted over a table inside it
(`RecordTypeRegistry::admit`, `crates/kernel/src/content/record_type.rs:181`). More
practically: an install with no daemon yet must still be able to enable the
plugin, run its gathers and show empty pages rather than error.

So `001_netgrasp_schema.sql` creates every `ng_` table with
`CREATE TABLE IF NOT EXISTS`, and what it creates is a **faithful copy of the
daemon's DDL** — same columns, same types, same order — with the guards added and
nothing else changed. `db_tables` names them all explicitly as well, so the
allowlist does not depend on `extract_created_tables` parsing.

It is a copy, not a convergence. An earlier version of this decision claimed the
migration "converges to the same schema whether the daemon or the plugin got
there first", achieved with a block of `ALTER TABLE … ADD COLUMN IF NOT EXISTS`.
That was wrong twice over: the two schemas disagreed on the primary key type, on
every timestamp and on four column names, none of which an `ADD COLUMN` can
reconcile — and `CREATE TABLE IF NOT EXISTS` over an existing table is a silent
no-op whatever its shape, so nothing would have reported the difference. **On a
shared install the daemon migrates first**, and the plugin's copy exists only for
an install that has no daemon yet.

Since nothing in the kernel can check that claim, the plugin checks it itself:
`crates/netgrasp-core/tests/daemon_schema_test.rs::the_plugin_migration_is_a_faithful_copy_of_the_daemons_schema`
applies the daemon's DDL and the plugin's migration to two scratch schemas and
compares `information_schema.columns` table by table. Every other test in that
file runs the plugin's real statements — the constants in
`netgrasp_core::queries` — against the daemon's DDL rather than against the
plugin's copy of it.

The `ng_` prefix is not renamed.

---

### Decision 9 — the assistant scopes are three, and every write is a proposal

Trovato 0.102 lets a plugin declare what can be configured **by conversation**:
`tap_assistant_scopes` says what, `tap_assistant_context` describes one of them,
and `tap_assistant_tool` answers the model's tool calls. Netgrasp declares three
scopes and no more, and the count is the decision.

- `netgrasp_device` (`id_kind: Item`, `ng_device`) — one device: its owner, its
  name, its notes, its two flags.
- `netgrasp_person` (`id_kind: Item`, `ng_person`) — one person: their name,
  notes and notification settings, the devices that are theirs, and deleting them.
- `netgrasp_network` (`id_kind: None`) — everything: who owns what, who is home,
  and the tidying-up that spans more than one thing.

A fourth scope per gather page was considered and rejected: a scope is a *thing
being configured*, and `/events/security` is a view of the same network the third
scope already is. Three scopes with overlapping tools beat eight with the same
tools split up, because the model picks the scope by URL and the person picks the
URL by what they are looking at.

**Every write is a proposal.** That is the kernel's design and not this plugin's,
but two consequences are the plugin's:

1. A write tool is dispatched with `mode: Describe` first, and **must change
   nothing** in that mode. Netgrasp's Describe branches do the same reads the
   Execute branches do — resolve the device, resolve the person, count the
   devices in the way — and then return a sentence. A host-in-the-loop test
   asserts a Describe leaves `ng_devices` and the Item byte-identical.
2. The sentence is the whole basis on which somebody clicks Apply, so every one
   of them names the device by its display name **and** its MAC, names the
   person, and states the value being replaced: *"Assign Amazon tablet
   (02:00:5e:00:00:04) to Jamie (currently Arlo)"*. `netgrasp_core::assist` builds
   those strings and is exhaustively tested without a database.
3. **An edit is sparse, and the card names its columns.** A tool call is about
   one thing — "rename this to Office printer" says nothing about the alerts —
   so `netgrasp_core::model::DeviceEdit` says, per user-owned column, either
   "set it to this" or nothing at all, and `writeback::build_partial_update`
   builds its `SET` list from the columns the edit names. The card's change set
   is `DeviceEdit::columns`, the same list that statement is built from, so the
   displayed change set and the executed change set are one value read twice.

   This is a correction, not a flourish. The first version built a **whole**
   overlay and filled the columns the call had not named from the device's Item
   — which the sync minted carrying only `field_mac`, and where an absent
   boolean reads as `false`. So a rename wrote `notify = false` over a column
   whose schema default is `TRUE`, twice in one run, and the card said nothing
   about it (`docs/JOINT-RUN.md`, plugin finding 1). The Item still has to be
   saved whole, because `Item::update` replaces `fields` wholesale; the **row**
   must not be.

**A device write may have to mint an Item first.** `write_back_device` addresses
the row by `trovato_item_id`, and only a `dirty` row ever gets an Item from the
cron sync — so a device that has been `clean` since before the plugin existed, or
that the demo seed created, has no Item and a naive write would update zero rows
and report success. Every device write therefore resolves the row, mints the Item
the way `sync_one` does when there is none, and says so on the card ("This also
creates its Trovato item"). It is the first thing this feature got wrong in
testing and the reason that test exists.

**A scope's context says when its data starts.** The network scope's snapshot
opens with the earliest observation the database holds and the current time; the
device scope's says the same about that device's `first_seen`. Asked who was
online yesterday against a database an hour old, a model finds zero presence
spans — the truth — and, told nothing else, explains it as a gap in monitoring,
which is what happened (`docs/JOINT-RUN.md`, plugin finding 3). An empty window
before the first observation and an empty window over a monitored period are the
same zero rows and different answers, and the context is the only place that
difference can be stated.

**Nothing an assistant does writes `sync_state`.** The two statements that write
it are both in `sync_host.rs` and neither is reachable from a tool, so Decision
4's termination argument is untouched: an applied proposal produces a `clean` row
the next sync pass does not select. A test asserts a following cron tick examines
zero rows.

**The permission is checked twice, and the two checks disagree by design.** The
kernel gates opening a conversation on `administer netgrasp` (or `administer
site`, which it treats as a superuser). Every tool checks `administer netgrasp`
again at the moment of the call, because a conversation outlives the request that
opened it. The host's `current-user-has-permission` has no `administer site`
bypass, so those two checks are not the same check —
`G-USER-API-NO-ADMIN-BYPASS` in `FRICTION.md` says so, and a site granting the
permission for real is the answer until the kernel's is.

---

### Decision 10 — the row menu exists without a right-click, and without JavaScript

Every listing row carries an actions menu. The menu is a `<details>` element and
every entry in it is a link; the browser opens it on click, on Enter and on
Space, and nothing in that path is scripted.

**Right-click is a convenience over a menu that already exists.** The
`contextmenu` handler in `static/js/netgrasp.js` opens the same `<details>` the
button opens, closes any other, and adds no entry, no destination and no
capability that is not reachable with scripting switched off. That is the test it
has to pass: if turning JavaScript off removed a way to do something, the feature
would be wrong, not merely degraded. Touch, which has no contextmenu event worth
binding, is why the button is visible rather than revealed on hover — on a touch
screen the button is the whole interface.

**No entry writes from the row.** Each one links to a page the plugin serves,
which renders the actual form. This is forced by the kernel and is the same
context limitation that put the auto-reload interval in a template literal: a
gather content template is rendered in its own Tera context —
`query`, `rows`, `total`, `page`, `per_page`, `total_pages`, `has_next`,
`has_prev`, `base_path`, `exposed_filters`, `filter_values`, `pager` — and the
site context carrying `csrf_token` is built afterwards, for the page wrapper
(`crates/kernel/src/routes/gather.rs`, `render_gather_with_theme`;
`routes/helpers.rs`, `inject_site_context`). A `<form method="post">` written into
a row would therefore post without a token and be refused by the kernel with 403
before this plugin was dispatched at all — a feature that could not have worked
rather than one that broke. `tap_api` is handed a freshly minted token in
`ApiRequest::csrf_token`, so the page the link opens can carry one.

The same gap decides two smaller things. "Assign owner" is a select of people,
and the people list is not in a device gather's context either, because a gather
reads one record type — so the page the link opens is also the first place that
select could be built. And an event row's menu acts on the **device** the event
is about, reached by the `device_id` the row does carry, because an event is a
read-only record with nothing of its own to configure (Decision 2).

---

### Decision 11: the overview is a one-row view with its lists as includes

The front page answers four questions at once: who is home and since when, who
came and went today, what is new on the network this week, and whether anything
suspicious is happening. A gather page renders one query and its template cannot
run another (Decision 10 lists its context), and a `gather_query` tile renders as
an empty `<div data-query-id>` for client script to fill, so neither composes a
page on the server.

`includes` does. The kernel runs each include as a child gather after the parent,
batched, and writes the child rows onto each parent row under the include's name.
So `/overview` is a gather over `ng_overview`, a view with exactly one row whose
columns are the counts, and its three lists (`home`, `movements`, `new_devices`)
arrive on that row. The counts are columns because a gather cannot count
(Decision 7); the lists are includes because a view with one row cannot hold a
page of rows without flattening them into JSON the templates would then have to
unpack.

Three of the four questions are relative to the clock, which is what put them in
views (`007_netgrasp_overview_views.sql`) rather than in gather filters. A filter
has `current_time` and `current_date` and no offset, so "seven days ago" is not a
value it can hold; and `current_date` is the kernel process's local date while
the rows are compared in the database's session time zone. A view evaluates
`now()` in the session that compares the rows, so "today" means one thing on the
whole page: the database's calendar day.

Four things follow, each held by a test:

- **An include joins on equal text.** `child_field IN (parent values)`, bound as
  text, and `child_field` is used both as a filter field (through the record
  field map) and as a row key (physical). So each join column is text with the
  same logical and physical name: a person's `state`, a movement's `day`, a new
  device's `period`. `period` is a constant the week-bounded view carries for
  exactly this, because "the last seven days" is a range and an include cannot
  join on one.
- **An include's name must not be a column of its parent.** The kernel writes the
  child list over whatever key was there, so an include named `people_home`
  would replace the count with a list.
- **Every list is also a page, and the two definitions agree.** An include
  carries its definition inline, so each is written twice; the test compares
  record type and filters (less the join) and allows the sort to differ only on
  movements, oldest first for one day and newest first for the log.
- **The views are the assistant's queries too.** The network scope's
  `arrivals_and_departures` and `new_devices` reads select from the same views
  (`netgrasp_core::queries::SELECT_MOVEMENTS_ON_DAY`, `SELECT_PEOPLE_HOME`,
  `SELECT_NEW_DEVICES`), and ask the database what day it is rather than
  computing one, so a page and a conversation cannot disagree about what
  "today" or "new" means. Both are reads; the scope's write tools are unchanged.
  The schema test runs the three statements over the daemon's DDL with 007 on
  top.

The front page moves to `/overview` only where `site_front_page` is still the
`/devices/online` 005 wrote, or unset. An operator's own choice is left alone,
the same promise 005 made from the other side.

---

## What is not in this build

- **No kernel modification.** Every friction item is reported, not fixed
  (`FRICTION.md`).
- **No daemon-side commit.** The daemon lives in the `netgraspd` repository
  (`github.com/jeremyandrews/netgraspd`); the `Code/netgrasp/` path named in the
  scope was never where it is, which is why no checkout was found when this was
  written. The plugin side of the two-writer contract is specified in
  `netgrasp_core::columns::USER_OWNED` and enforced by test.
  The daemon-side half of it ("verify the daemon tolerates user-owned columns
  being written by another process") has since been run, against a live LAN with
  both halves on one database: it holds. The daemon's single device-update
  statement names no user-owned column, and `display_name`, `notify` and
  `owner_item_id` written through the assistant survived three hours of daemon
  flushes untouched. What that run also showed is that the daemon reads those
  columns only when it starts, so a change made through the assistant does not
  reach a running daemon until it restarts. See `docs/JOINT-RUN.md`.
- **No enrichment of its own, no arrival-departure notification** (CLOSE 16),
  **no iOS.** The plugin talks to no UniFi controller. It shows what the
  daemon's enrichment wrote (`current_location`, `current_ap`, the location
  history) wherever the daemon wrote it, and with enrichment off every one of
  those is null: the device tables then have no Where column at all, the device
  page leaves out its Location and Access point rows and says where a location
  would come from, and `/devices/location` is its empty state saying the same.
- **No assistant scope over the event log.** Events are read-only and high volume:
  there is nothing to configure, and the network scope's `device_history` already
  puts a device's events in front of the model.
