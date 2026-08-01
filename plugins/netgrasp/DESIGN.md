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
| `ng_device` Item | the user's overlay: label, owner, notes, hidden, notify | an admin, through the kernel's content forms; created once by sync |

The two column sets are **fixed and disjoint**, which is what makes "the two
writers never collide" a schema property rather than a promise:

- **daemon-owned:** `mac`, `hostname`, `vendor`, `device_type`, `os_family`,
  `state`, `last_ip`, `current_location`, `first_seen`, `last_seen`, `sync_state`
- **user-owned:** `display_name`, `owner_item_id`, `notes`, `hidden`, `notify`
- **link-owned (plugin):** `trovato_item_id`

`netgrasp_core::sync::USER_OWNED_COLUMNS` names the second set once, the
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
  `DELETE FROM ng_events WHERE timestamp < $1` per cron tick. As Items it is one
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

The Item's title is `COALESCE(display_name, hostname, vendor || ' device', mac)`
(`netgrasp_core::sync::derive_title`), which is the only thing the daemon side has
to say about an Item whose fields are otherwise all user-owned. This is why the
update branch has real work and is not scaffolding.

**Kernel → daemon** (`tap_item_update` on `ng_device`): writes exactly
`USER_OWNED_COLUMNS` into `ng_devices WHERE trovato_item_id = $id`, and
`display_name` takes the Item's title. It does **not** write `sync_state`.

One subtlety, found by the integration test rather than by design, and worth
recording because the naive version is silently wrong: **an unchanged title must
clear `display_name`, not store it.** `display_name` outranks `hostname` in
`derive_title`, so if every save stored the title, an admin who edited only the
*notes* of a device still called `aa:bb:cc:dd:ee:ff` would pin that MAC as its
label forever — the daemon could resolve a hostname the next minute and the
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

### Decision 7 — the tiles are gather tiles, because a tile cannot count

`G-NO-GATHER-AGGREGATION` is unchanged: `QueryDefinition` has no aggregate
projection and no tile type computes anything. "Online count" is therefore a
`gather_query` tile over `ng_device_online` whose **pager count** is the figure
and whose rows are what is behind it. Same for who-is-home, recent events and
security alerts. The shape is imposed, not chosen.

### Decision 8 — the plugin's migration creates the `ng_` schema, additively

The daemon owns these tables at runtime, but the plugin's effective DB allowlist
is `migration-owned ∪ db_tables` (`crates/kernel/src/plugin/db_policy.rs`), and a
record type is only admitted over a table inside it
(`RecordTypeRegistry::admit`, `crates/kernel/src/content/record_type.rs:181`). More
practically: an install with no daemon yet must still be able to enable the
plugin, run its gathers and show empty pages rather than error.

So `001_netgrasp_schema.sql` creates every `ng_` table with
`CREATE TABLE IF NOT EXISTS` and adds the plugin-owned columns with
`ALTER TABLE … ADD COLUMN IF NOT EXISTS`. It converges to the same schema whether
the daemon or the plugin got there first, and re-running it is a no-op either way.
`db_tables` names them all explicitly as well, so the allowlist does not depend on
`extract_created_tables` parsing.

The `ng_` prefix is not renamed.

---

## What is not in this build

- **No kernel modification.** Every friction item is reported, not fixed
  (`FRICTION.md`).
- **No daemon-side commit.** The `Code/netgrasp/` checkout named in the scope does
  not exist on this machine; `~/devel/rust/netgrasp-rs` is an empty tree with a
  `.git` directory and no working files, and no other copy is present. The
  daemon-side task ("verify the daemon tolerates user-owned columns being written
  by another process") could not be run. The plugin side of that contract is
  specified in `netgrasp_core::sync::USER_OWNED_COLUMNS` and enforced by test, so
  the daemon-side check is a bounded follow-up rather than an open question.
- **No enrichment / UniFi, no arrival-departure notification** (CLOSE 16), **no iOS.**
