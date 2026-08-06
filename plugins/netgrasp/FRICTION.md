# Netgrasp — Friction Log

Produced by building the Netgrasp plugin against the **frozen** PF-5 contract.
Argus met the kernel as a pipeline (M1), an intelligence engine (M2), a UI
consumer (M3) and a notifier (M4). Netgrasp meets it as something none of those
were: a **second writer**. There is an external process — the native netgrasp
daemon — that owns rows in the same database, and the plugin's whole job is to
put a human's edits in front of it and its observations in front of the human.
That is a different kernel again, and the findings below are the ones that shape.

Every item is severity-tagged with `file:line` evidence and phrased as a concrete,
decidable post-1.0 ledger item. **NEW** findings were surfaced by this build;
**RESIDUAL** ones are re-confirmed from this consumer's side. No-friction findings
are last, as required.

Verified at `KERNEL_API_VERSION (1,0)` (`crates/kernel/src/plugin/mod.rs:51`)
with **no kernel, WIT, SDK or kernel-migration change** in the build session. The
design decisions these findings forced are argued in `DESIGN.md`.

The last three findings (`G-DB-HOST-TYPE-COVERAGE`, `G-RECORD-ID-MUST-BE-UUID`,
`G-RECORD-STRUCTURAL-COLUMNS-UNVALIDATED`) were added by a later pass that
pointed the plugin at the daemon's **landed** schema for the first time, rather
than at the design record it had been built from. They are the findings a second
writer only meets once it has actually met the other writer, and the first of
them is load-bearing: it is why every timestamp in this plugin is read from a
generated companion column rather than from the column it belongs to.

Three findings are load-bearing rather than cosmetic. **G-SAVE-ITEM-BYPASSES-SERVICE**
is why plugin-written Items are never embedded and, by accident, why this
plugin's sync loop cannot start. **G-EMBED-OPTOUT-IS-NOT-AN-OPTOUT** is why the
scope's stated reason for making events Items does not exist. **G-TWO-WRITER-NO-CONTRACT**
is the one this milestone was really about.

---

## Findings

### G-SAVE-ITEM-BYPASSES-SERVICE — **[High, NEW]** `save-item` skips `ItemService`, so a plugin's Item write fires no taps and is never indexed

The `item-api` host function calls the **model** directly —
`Item::update` / `Item::create` (`crates/kernel/src/host/item.rs:132,163`) — not
`ItemService::create` / `ItemService::update`. Everything `ItemService` wraps
around a save is therefore skipped on the plugin path:

- `tap_item_presave`, `tap_item_insert`, `tap_item_update` do not fire
  (`crates/kernel/src/content/item_service.rs:323,354,553`);
- `index_item` does not run (`item_service.rs:603`), so `tap_item_update_index`
  never fires and **no embed job is enqueued**;
- the file-reference index is not maintained (`item_service.rs:sync_file_references`);
- the item cache is not invalidated.

**This is the mechanism behind M2's `G-ITEM-NO-EMBED`.** M2 recorded the symptom
("stories have no embeddings, so `/stories` is a plain recency list and semantic
gather needs a manual admin backfill") and M3 re-confirmed it as a residual. The
cause is one line of routing: an Item created by a plugin is invisible to the
kernel's own indexing because the host function never asks for it.

**Impact for Netgrasp, and it cuts both ways.** The *good* half is that the
daemon→kernel sync cannot trigger the kernel→daemon write-back, so the sync loop
terminates by construction rather than by discipline — a stronger guarantee than
this plugin could have built for itself. The *bad* half is that the guarantee is
an accident of an omission, and the obvious fix for the embedding defect closes
the loop. So the plugin also carries the discipline that would make it terminate
anyway (the write-back cannot emit `sync_state`), and
`crates/kernel/tests/netgrasp_sync_test.rs::the_plugins_own_save_item_fires_no_tap_which_is_why_the_sync_cannot_start_the_loop`
pins the kernel behaviour with a failure message naming this finding, so the day
it is fixed a test explains what changed rather than merely breaking.

**Recommendation (post-1.0):** route `save-item` through `ItemService` so plugin
writes are indexed and observable like any other, and treat the tap re-entrancy
that then becomes possible as a first-class concern (a plugin's tap firing on its
own write is a loop unless the plugin is careful, and nothing today tells an
author that). If full routing is too large, the minimum is to enqueue the embed
job — that alone closes `G-ITEM-NO-EMBED` without introducing re-entrancy.

### G-EMBED-OPTOUT-IS-NOT-AN-OPTOUT — **[Medium, NEW]** there is no way to say "do not embed this content type", and a plugin cannot say anything about embedding at all

`EmbedPolicy` (`crates/kernel/src/services/embed_index.rs:64-95`) has one field,
`sync_types: Vec<String>`, and `is_async()` returns whether a type is *absent*
from it. A type listed there does not skip embedding: it opts out of the **async**
path and takes the pre-P11f **synchronous** embed on the save path instead
(`crates/kernel/src/content/item_service.rs:653-655`). Both branches call the
provider. There is no third value meaning "never embed this type".

It is also not plugin-declarable. The policy is a `site_config` key
(`EMBED_POLICY_CONFIG_KEY`, `embed_index.rs:62`; `EmbedPolicy::load` at `:83`) and
`PluginInfo` has no embedding section at all
(`crates/kernel/src/plugin/info_parser.rs`).

**Impact.** This build's scope asserted that per-content-type embed opt-out
dissolves the "one embed per high-volume Item" cost and instructed the plugin to
"opt `ng_event` out of embedding in this plugin's declaration". Neither half is
expressible. The cost concern happens to be moot today for a different reason
(G-SAVE-ITEM-BYPASSES-SERVICE), which means a plugin author reasoning about the
cost of Items from the documented policy will reach a conclusion that is right by
coincidence and will stop being right the moment the routing defect is fixed.

**Recommendation (post-1.0, small):** give `EmbedPolicy` a third state
(`never_types`, or make `sync_types` a per-type enum of `async | sync | never`),
and let a plugin declare a default for its own types in `{name}.info.toml` that
the site config can override. A content type whose rows are machine-generated,
never searched semantically, and deleted on a retention timer should be able to
say so once rather than requiring every operator to know.

### G-TWO-WRITER-NO-CONTRACT — **[Medium, NEW]** nothing in the kernel expresses "this column belongs to someone else"

Netgrasp's core problem is that `ng_devices` has three writers — the daemon, the
admin (through the plugin), and the plugin's own linkage — and the kernel offers
no way to say so. The mechanisms that exist all stop at the wrong boundary:

- `db_tables` is table-granular (`crates/kernel/src/plugin/db_policy.rs`,
  `check_table`), so a plugin either may write every column of a table or none of
  it. There is no column-granular allowlist.
- `tap_field_access` (FR-8) governs **Item fields**, not record columns, so it
  cannot express "the plugin may not write `ng_devices.state`".
- Record types are read-only in admin (`routes/admin_record_type.rs:181-183`) and
  have no write surface at all, so there is nothing for a write policy to attach
  to even in principle.
- With `raw_sql = true` — which any plugin doing real work with the record tier
  needs — the plugin can write anything in any table it declares. The
  manifest documents that this "weakens the table-allowlist guarantee"
  (`info_parser.rs:189-195`), and column ownership is exactly the case where the
  weakening bites.

**Impact.** The scope's requirement that "the user-owned columns are a fixed
disjoint set from the daemon-owned columns so the two writers never collide" is
enforceable only *inside the plugin*. Netgrasp does enforce it — one constant
(`crates/netgrasp-core/src/columns.rs`), a statement builder that generates its
`SET` list by iterating it and therefore cannot name a column outside it
(`crates/netgrasp-core/src/writeback.rs`, `build_update`), a disjointness unit
test, and an integration test that snapshots every daemon column across an
adversarial edit. That is a good implementation of a discipline, and it is still
just a discipline: the next plugin to share a table with an external process will
invent its own, or will not.

**Recommendation (post-1.0):** a `[[db_tables]]` long form carrying
`writable_columns` (defaulting to all, so nothing changes for existing plugins),
enforced on structured calls and — the part that matters for a `raw_sql` plugin —
checked against the `SET`/`INSERT` column lists of raw statements. Even without
raw-SQL enforcement, the declaration alone would make the intent reviewable
instead of buried in a plugin's own constant.

### G-NO-RECORD-WRITE-SURFACE — **[Medium, RESIDUAL, new consequence]** the record tier's read-only admin forces a second tier for anything editable

M1 filed the read-only record admin as "no hit — exactly right for operational
inspection". M3 filed it as "the single fact that decided the design gate". For
Netgrasp it decides the **data model itself**, and produces a shape neither
milestone had to build: a device is *two rows in two tiers*, a record for its
observed state and an Item for its editable overlay, because there is no one tier
that is both cheap enough for per-sighting churn and writable by a human.

The cost is not conceptual, it is concrete: an id linking the tiers
(`trovato_item_id`), a sync pass to maintain it, a relink path for when an
operator deletes the Item, a title derivation that has to be a fixed point across
both directions, and a device page that has to fetch from the record tier because
the Item does not hold the data (`DESIGN.md` Decisions 1, 4, 5). Roughly half
this plugin exists to bridge a gap that a writable record tier would close.

**Recommendation (post-1.0):** either an editable record admin gated on a declared
per-type permission, or the `tap_page`/`tap_api` dispatch M3 recommended
(`G-NO-PLUGIN-HTTP`) — which would let a plugin own a small edit form over its own
table. Either collapses the two tiers into one for this class of plugin.

### G-NO-GATHER-AGGREGATION — **[Medium, RESIDUAL, worse here]** "how many devices are online" cannot be a number

Unchanged from M3: `QueryDefinition` has no grouping or aggregate projection
(`crates/kernel/src/gather/types.rs:15-76`) and no tile type computes anything.

It is worse for Netgrasp than it was for Argus, because Netgrasp's headline
figures are *counts by nature*: how many devices are online, and who is home. The
"who is home" tile is the sharp case — the answer is a list of **people**, and the
data is a list of **devices**, so without a `GROUP BY` a person with three devices
appears three times. The seeded gather sorts by owner so the repetition at least
reads as grouping (`002_netgrasp_gathers.sql`, `ng_who_is_home`), which is a
presentation apology for a missing query feature.

**Recommendation (post-1.0):** an aggregate projection on `QueryDefinition`
(`{"field": …, "function": "count", "group_by": […]}`), or at minimum a `count`
tile type rendering a gather's total without its rows.

### G-EXPOSED-FILTER-NO-MATCH-ALL — **[Medium, RESIDUAL, worse here]** a blank exposed filter over a record uuid column raises, and this plugin's facets are uuid columns

Unchanged from M3 (`crates/kernel/src/gather/gather_service.rs:1060-1073`,
`crates/kernel/src/gather/query_builder.rs:500-503`). Recorded again because the
severity is consumer-dependent and Netgrasp is the bad case: its two natural
facets are **owner** and **device**, both uuid columns on a record gather, so a
"leave blank for all" exposed filter does not return an empty page — it returns a
500, in the page's default state.

The consequence is structural rather than cosmetic: the plugin ships **no exposed
filters at all**, and every facet is a separate route with a `{"url_arg": …}`
value that is always supplied (`DESIGN.md` Decision 6). A device list a user can
narrow interactively is not expressible; a set of pre-built links is.

**Recommendation (post-1.0):** skip an exposed filter whose resolved value is
empty, for every operator — matching what `in`, `not_in` and `full_text_search`
already do (`query_builder.rs:564,571,581`).

### G-DISPLAY-CONFIG-CANNOT-STYLE-A-ROW — **[Low, NEW]** a gather's display JSON has no per-row conditional styling

A `QueryDisplay` carries `format`, `items_per_page`, `pager`, `empty_text`,
`header` and `footer` (`crates/kernel/src/gather/types.rs`, `QueryDisplay`) and
nothing that varies by row.

**Impact.** The scope asked for security events to be "styled distinctly via
display config" inside the main event log. That is not expressible: there is no
way to say "give a row whose `event_type` is `mac_spoof` a different class". The
plugin ships a separate `/events/security` route instead
(`002_netgrasp_gathers.sql`, `ng_event_security`), which is the honest version of
the same intent and is also what the alerts tile points at — but a single log
where the alarming lines stand out is a better answer for an operator scanning
it, and it is not available.

**Recommendation (post-1.0, small):** a `row_class` entry on `QueryDisplay`
mapping a field value to a CSS class, or a `{field, equals, class}` rule list.
Purely presentational and additive.

### G-SDK-NO-ITEM — **[Low, RESIDUAL, now duplicated]** the SDK still ships no `item-api` binding, and two plugins now carry the same hand-rolled one

M2 recorded that `crates/plugin-sdk/src/host.rs` has externs for db, ai, http,
logging, queue, crypto, variables, user and plugin invocation and **nothing** for
items, even though `item-api` is a valid manifest capability and the kernel
registers the interface (`crates/kernel/src/host/item.rs`).

The finding is unchanged; what is new is that it has now been paid for twice.
`plugins/netgrasp/src/item_host.rs` is the same declaration, the same calling
convention, the same 256 KB buffer constant and the same native stubs as
`plugins/argus/src/item_host.rs`. Two independent plugins writing identical unsafe
FFI is the point at which "a gap" becomes "a defect".

**Recommendation (post-1.0, trivial):** export the binding from the SDK.

### G-SDK-NO-ESCAPE — **[Low, RESIDUAL, higher stakes here]** the SDK ships no HTML escaping helper, and this plugin renders attacker-supplied text

M3 recorded that a plugin rendering anything must escape it, that the kernel does
not escape view output, and that `crates/plugin-sdk/src/` provides no helper while
the kernel keeps a good one to itself in `crate::routes::helpers::html_escape`.
Argus wrote its own; Netgrasp has now written the same function again
(`plugins/netgrasp/src/device_view.rs`, `escape`).

Worth restating because the stakes differ. Argus escapes model output over fetched
articles. Netgrasp escapes **a hostname**, which is DHCP option 12 — whatever an
unauthenticated device on the LAN claims — plus access-point names and vendor
strings. A device page is the one place in a Trovato install where an
unauthenticated party on the local network gets to put characters in front of an
administrator. "Every plugin that renders will write it again, and some will write
it wrong" is a worse sentence in that setting.

**Recommendation (post-1.0, trivial):** export `html_escape` from the SDK.

### G-VIEW-OUTPUT-JSON-ENCODED — **[High, RESIDUAL]** view-tap HTML still reaches the page JSON-encoded

Unchanged from M3 (`crates/plugin-sdk-macros/src/lib.rs:158`;
`crates/kernel/src/routes/item.rs`, "Include plugin render outputs"). Netgrasp
inherits the defect and the mitigation verbatim: single-quoted attributes and an
escaper emitting `&quot;`/`&#x27;`/`&#x5C;`, so the fragment contains no character
serde would escape. Unit tests assert the fragment is free of `"` and `\` on every
render path, and
`netgrasp_sync_test.rs::the_device_pages_view_output_is_json_encoded_by_the_contract`
pins the kernel behaviour.

Recorded again only to note that the mitigation is now being copied between
plugins, which is how a workaround becomes a convention.

### G-DB-HOST-TYPE-COVERAGE — **[High, NEW]** the `db` host decodes nine Postgres types and silently nulls everything else, and the gather path disagrees with it

Added after pointing the plugin at the daemon's landed schema for the first time.
The plugin had been built from the design record rather than from the daemon's
migrations, and reconciling the two turned up a defect underneath the column
names that decided the shape of the whole fix.

`row_to_json` in `crates/kernel/src/host/db.rs:104-161` matches on the column's
Postgres type name and handles nine: `BOOL`, `INT2`, `INT4`, `INT8`, `FLOAT4`,
`FLOAT8`, `UUID`, `JSON`, `JSONB`. Everything else falls through to
`row.try_get::<String, _>(name).ok()`. For `TEXT` and `VARCHAR` — the comment's
stated intent — that is right. For every other type it is a **silent null**:
`.ok()` discards the decode error, so a `timestamptz`, a `numeric`, a `date`, an
`inet`, a `bytea` or any array arrives at the plugin as `null` and is
indistinguishable from a column that is genuinely null. That path serves the
structured `select` and `query_raw` alike.

The gather path is not better, it is **different**: it wraps the query in
Postgres' own `row_to_json` (`crates/kernel/src/gather/gather_service.rs`,
`crates/kernel/src/routes/admin_record_type.rs:78-88`), so the same
`timestamptz` column arrives there as an ISO 8601 string. So one column has three
renderings depending on which door you came through — `null`, `"2026-08-06T…"`,
and unreachable — and none of them is the integer a plugin renders a duration
from.

**What it cost this plugin.** Everything the daemon records about *when* is a
`timestamptz`: six columns across five tables. A plugin over a schema it does not
own cannot change those columns, and the two workarounds available on the plugin
side both fail:

- `SELECT last_seen_at::text` returns a string the plugin would have to parse,
  and a wasm guest has no timezone database to parse it with;
- `SELECT EXTRACT(EPOCH FROM last_seen_at)::bigint` works and is what a
  `raw_sql` plugin can do — but the record tier's field map and the gather
  definitions take **column names**, not expressions, so the gathers, the record
  admin and the facets cannot use it. Only the plugin's own hand-written SQL
  could.

The answer landed on was a schema change on the **daemon's** side: every
`timestamptz` gains a generated `<column>_epoch` twin,
`BIGINT GENERATED ALWAYS AS (EXTRACT(EPOCH FROM (col AT TIME ZONE 'UTC'))::bigint)
STORED`. The plugin reads the twin and aliases it back to the name its row struct
expects; the record field maps name the twin; the gathers sort on it.

**Is the epoch-companion pattern a reasonable general answer?** For this plugin,
yes, and it is better than it sounds: the twin is `GENERATED ALWAYS … STORED`, so
no writer on either side can put a wrong value in one, it is indexable, and it
costs 8 bytes a row. It is also the only shape that makes the structured host and
the gather path agree, which no expression-level workaround does.

As a **general** answer it is not reasonable, for one reason: it requires the
plugin to be able to change the other process's schema. Netgrasp could, because
the daemon and the plugin ship together. A plugin over a schema it genuinely does
not control — a vendor's table, a replica, a view — has no such move, and for it
the finding is simply "the kernel cannot read your timestamps". Six extra columns
in someone else's migration is a workaround with a co-author, not a pattern.

**Recommendation (post-1.0):** extend `row_to_json`'s match with the types
Postgres actually returns — `TIMESTAMPTZ`/`TIMESTAMP` as unix seconds (an
integer, matching how the kernel stores its own `created`/`changed`), `DATE`,
`NUMERIC` as a string, `INET`/`CIDR`/`MACADDR` as strings, `BYTEA` as base64 —
and make the fall-through **loud**: a type the host cannot decode should return
`ERR_SQL_FAILED` or a typed error rather than a null that reads as data. The
silent null is the worse half of this finding by some distance. It is also worth
deciding that the gather path and the `db` host render a column the same way;
today a plugin author has to know which door a value came through to know what
type it will be.

`crates/netgrasp-core/tests/daemon_schema_test.rs::a_timestamptz_is_null_through_the_db_host_and_its_epoch_twin_is_not`
pins the behaviour with a failure message naming this finding, so the day the
host grows a `TIMESTAMPTZ` arm, a test says the companion columns can go.

### G-RECORD-ID-MUST-BE-UUID — **[Medium, NEW]** the record tier assumes a uuid primary key, so a record over a native writer's `bigint` table can be listed but never viewed

`ng_devices.id` is `BIGINT GENERATED ALWAYS AS IDENTITY`, because the daemon's
tables were designed before Trovato was in the picture and a bigint identity is
what a native process watching a LAN reaches for. Nothing in the record-type
declaration objects: `RecordTypeRegistry::admit`
(`crates/kernel/src/content/record_type.rs:172-200`) checks the table allowlist
and the name collision, and nothing else. The listing works, because it projects
`{id_column}::text`.

The per-row view does not. `view_record` takes
`Path<(String, uuid::Uuid)>` (`crates/kernel/src/routes/admin_record_type.rs`),
so `/admin/structure/records/ng_device_state/42` fails to match the route and
404s. Four of this plugin's six record types are bigint-keyed and none of their
rows can be opened; the declaration's own doc comment says "Primary-key column
(UUID)" (`crates/kernel/src/plugin/info_parser.rs:106`), so the assumption is
documented — it is just not enforced anywhere a plugin author would meet it.

**Impact.** Mild for Netgrasp: the device page is an Item page, and the record
admin is an inspection surface. It is a real limit on the record tier's stated
purpose, though — "a table this plugin did not create" is exactly the case where
the primary key is not the kernel's choice to make.

**Recommendation (post-1.0):** accept a `String` in the path and let the record
type's own id column decide the cast, or — cheaper and honest — reject a record
type at registry build whose `id_column` is not a uuid, so the failure is a
startup error naming the plugin instead of a 404 nobody attributes.

### G-RECORD-STRUCTURAL-COLUMNS-UNVALIDATED — **[Low, NEW]** `created_column` / `changed_column` default to columns most tables do not have, and the listing orders by one of them

`RecordTypeDecl` defaults `id_column` to `id`, `created_column` to `created` and
`changed_column` to `changed` (`crates/kernel/src/plugin/info_parser.rs:106-119`).
No daemon table has a `created` or a `changed`, and the record admin's listing is
`ORDER BY {changed_column} DESC` interpolated straight into the SQL
(`crates/kernel/src/routes/admin_record_type.rs:78-88`). A record type that
simply omits the field — which is the natural thing to do when the concept does
not apply — therefore registers cleanly and 500s the moment an operator opens its
list. Netgrasp's six record types did exactly that until this pass; they now name
real columns explicitly, and `ng_person_mirror` has to point `created` at a
last-arrival timestamp because `ng_people` records no creation time at all.

Nothing validates that a declared column exists, either — not the structural
columns and not the field map. The registry has no database handle at build time,
which is a fair reason, but the consequence is that a typo in a field map is a
runtime SQL error inside a gather rather than a startup error naming the plugin.

**Recommendation (post-1.0):** make the structural columns `Option` with no
default and skip the `ORDER BY` when `changed_column` is absent; and validate
declared columns against `information_schema` once at startup, after migrations,
where the answer is cheap and the error can name the plugin, the record type and
the column.

---

## Residual findings re-confirmed from this consumer's side

- **[Medium, RESIDUAL] `G-ITEM-NO-MERGE`.** `Item::update` reads `fields` as
  `input.fields.unwrap_or(current.fields)` (`crates/kernel/src/models/item.rs:295`)
  — whole-object replacement. Met from a new direction, and for once turned into
  an *advantage*: because omitting the key entirely means "leave them alone", the
  sync's title refresh sends `{id, title}` and provably cannot clobber an admin's
  edit, which is what let the design avoid a read-modify-write it could not have
  made safe (see `G-DB-NO-TX`). The finding stands — a partial-field update is
  still inexpressible — but this consumer found the useful half of it.
- **[Medium, RESIDUAL] `G-DB-NO-TX`.** Shapes the sync's ordering: with no
  transaction, the Item is written *before* `sync_state` is cleared, so a pass
  that dies between the two leaves the row dirty and the next pass redoes it.
  Every step is an upsert or a no-op, so redoing it is free. The plugin is
  correct under interruption because it was designed around the absence, not
  because the absence did not matter.
- **[Medium, RESIDUAL] `G-ADMIN-UI-IS-ADMIN-ONLY`.** Every `/admin/content/...`
  route is gated on `users.is_admin` (`crates/kernel/src/routes/helpers.rs:74-91`),
  so the seeded `network_admin` role can manage devices through the JSON item
  routes and not through the screens. For a home-network tool this is the ordinary
  case, not the exotic one: "let my partner rename devices without making them a
  site administrator" is the whole point of the role.
- **[Medium, RESIDUAL] `G-NO-PRESAVE-VETO`.** `tap_item_presave` can modify but
  not refuse (`item_service.rs:308-345`). Netgrasp coerces rather than validates:
  a MAC is normalised to lower-case colon form, and an owner that is not
  uuid-shaped is **blanked** rather than kept, because it would otherwise reach
  `owner_item_id` — a uuid column — and fail the write-back in a background tap
  with a cast error the admin never sees. The enforcement lands one layer from the
  mistake instead of at it.
- **[Medium, RESIDUAL] `G-ITEM-FORM-MISMATCH`.** `FieldType::RecordReference` does
  not survive an admin edit, so the device's owner is a plain `Text` uuid an admin
  pastes. This is a worse outcome for Netgrasp than it was for Argus: a feed's
  topic is set once by a technical operator, whereas assigning a device to a
  person is the routine domestic action the plugin exists to support, and "paste
  this uuid" is not a thing to ask of the person doing it. The
  pre-existing netgrasp skeleton declared `owner_id` as a `RecordReference`; this
  build reverses that (nothing had ever been stored against it).
- **[Medium, RESIDUAL] `G-NO-PLUGIN-HTTP`.** Unchanged, and the reason the device
  page is a `tap_item_view` fragment rather than a route, the reason the facets
  are aliases rather than handlers, and the reason `tap_menu` entries here set no
  `callback` (the SDK's field is dropped on deserialize —
  `crates/plugin-sdk/src/types.rs:575,595` vs
  `crates/kernel/src/menu/registry.rs:13-42`). The skeleton set two callbacks,
  which did nothing; they are gone, and a unit test now forbids them.
- **[Medium, RESIDUAL] 64 KB tap I/O buffer.** Held with no workaround needed, and
  is **not** the binding constraint the scope expected. Device rows never cross a
  tap boundary — they are read through the `db` host and written through
  `item-api` — so the only thing crossing is the sync report's handful of
  integers. The ceiling that actually binds is the SDK's 256 KB `query_raw` output
  buffer, which sets the 200-rows-per-tick page size.
- **[Medium, RESIDUAL] cron cadence is external-only.** `tap_cron` fires with a
  `{timestamp}` and no cron key, so a plugin multiplexes its duties inside one
  tap. Netgrasp has two (sync, retention) and does exactly that. Minor, as M1 said.
- **[Low, RESIDUAL] 5 s DB statement timeout.** One hit, and it was anticipated:
  the retention `DELETE` is bounded to 5,000 rows per tick
  (`crates/netgrasp-core/src/retention.rs`) so a long-neglected install drains over
  successive ticks rather than timing out forever on one enormous statement.

---

## No-friction findings (surfaces that just worked)

- **The lightweight record tier over a table this plugin did not create.** Six
  record types over daemon-owned tables were admitted first try, and the
  `db_tables` ∪ migration-owned allowlist derivation
  (`crates/kernel/src/plugin/db_policy.rs`) meant an idempotent
  `CREATE TABLE IF NOT EXISTS` migration made the plugin installable with or
  without a daemon present. The record tier is genuinely the right home for
  events, and gather-over-record with real column types and real indexes is a
  different quality of thing from gather-over-JSONB.
- **The record-type / content-type namespace check.** `RecordTypeRegistry::admit`
  rejecting a record type whose name collides with a content type
  (`crates/kernel/src/content/record_type.rs:189-191`) is what forced the device's
  two tiers to be named `ng_device` and `ng_device_state`. The failure would
  otherwise have been a confusing gather resolution bug much later. Exactly the
  right check in exactly the right place.
- **`ContextualValue::UrlArg` over a record column.** `/devices/owner?owner=<id>`
  is one line of gather definition and worked against a real uuid column first
  try. It is the reason the facet-as-a-route workaround for
  `G-EXPOSED-FILTER-NO-MATCH-ALL` is tolerable rather than painful.
- **Plugin migrations seeding kernel tables.** Gathers, URL aliases, roles, role
  permissions and tiles all seed with plain `INSERT … ON CONFLICT`, which is what
  lets this plugin ship a whole surface — eight routes, four tiles, two roles —
  rather than a schema and a README.
- **`tap_item_insert` / `tap_item_update` / `tap_item_delete` on the admin path.**
  The full lifecycle fired as documented from `ItemService`, with a live services
  handle, which is what makes the person mirror and the device unlink possible at
  all. The asymmetry with `save-item` is the finding above; the admin-path
  behaviour itself is exactly right.
- **The WASM-1 capability gate.** Declaring four host interfaces and getting
  exactly those, with `raw_sql` as a separate explicit switch, remains the right
  shape. Netgrasp needs no `http`, no `ai-api`, no `queue` and no `user-api`, and
  saying so in four lines is worth more than any amount of documentation about
  what a plugin does not do.
- **`raw_sql` as the shock absorber between two schemas.** When the plugin's
  idea of the daemon's schema turned out to be wrong in every dimension that
  matters — the primary key type, every timestamp, four column names, and one
  column's type — the plugin absorbed all of it by projecting and renaming on
  read (`SELECT started_at_epoch AS start`, `SELECT ip AS label`), so the
  rendering code and every row struct stayed where they were. A structured-only
  plugin would have had no such move and the daemon would have had to rename its
  columns to suit the kernel. That `raw_sql` is a separate, explicit, auditable
  switch rather than an implication of `db` is what makes it usable for this:
  the manifest says out loud which plugins can do it.
- **The host-agnostic core split.** `netgrasp-core` holds the sync plan, the
  column discipline, the retention window and the timeline arithmetic behind no
  host at all: 65 unit tests, no database, no wasm. The two properties this
  milestone had to prove — that the sync is idempotent and that the loop
  terminates — are proved as pure functions there and then confirmed end to end
  against Postgres. The pattern Argus established paid for itself a second time.
