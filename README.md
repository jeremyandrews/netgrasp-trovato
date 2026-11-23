# Netgrasp for Trovato

The web interface for the [netgrasp daemon](https://github.com/jeremyandrews/netgraspd): a
[Trovato](https://github.com/jeremyandrews/trovato) plugin that turns the tables the daemon
writes into pages you can leave open on a wall screen. Devices, people, presence,
and a security log that flags a spoof where you will actually see it.

The daemon watches a LAN and writes `ng_`-prefixed tables. This repository makes
those rows visible and editable through Trovato, and does nothing on the network
itself.

## The demo, in one command

Docker and nothing else: no Rust toolchain, no Trovato checkout, no LAN, no
daemon.

```bash
docker compose -f docker-compose.demo.yml up
```

Then open **http://localhost:3101/**, which redirects to `/overview`. The
[four-entry navigation](#the-pages) reaches every page from there.
`scripts/verify-demo.sh` checks the same thing from a terminal, and CI runs it
against this demo.

The first run compiles the plugin inside a container and pulls the published
kernel image, so it takes a few minutes. After that it is seconds. What comes up:

| | |
|---|---|
| kernel | the published image, unmodified, at the version in `kernel-release.toml` |
| netgrasp | the plugin, the templates and the assets, mounted read-only on the three search paths |
| rows | `scripts/seed-demo.sql`: 24 devices (2 hidden), 3 people, 27 events, 6 of them security |
| clock | a container POSTing `/cron/<CRON_KEY>` every 60 seconds, because the kernel runs no scheduler |

Nothing is copied into Trovato and nothing is written to your clone: the
assembled plugin directory lives in a Docker volume, and `templates/` and
`static/` are mounted from the repository read-only.

`docker compose -f docker-compose.demo.yml down -v` removes all of it, volumes
included. `up --build` rebuilds the plugin after an edit. The compose file
documents the ordering a brand new database needs, and the one expected warning
on first run.

### Who sees device data

Signing in is required. A visitor who is not signed in gets the login page, on
every netgrasp page, every Item page and every item, search and gather API.
Two migrations make it so. `013_netgrasp_home.sql` takes `view netgrasp devices`
from the anonymous role (which `005` had granted it for a localhost dashboard),
and the plugin's `tap_field_access` denies every field of every netgrasp record
type, and of its two Item types, to a viewer without it.
`025_netgrasp_private_site.sql` takes `access content` from the anonymous role
too, because the kernel serves Item pages, item lists, search and the JSON of
two gathers on paths no field decision reaches. In the demo, sign in as `admin`
with the password in `docker-compose.demo.yml`. The `network_viewer` role holds
the permission for everyone else.

A wall screen or a trusted LAN that wants the old open view grants both back.
Run it once; the migrations never run a second time, so it will not be undone:

```sql
INSERT INTO role_permissions (role_id, permission)
VALUES ('00000000-0000-0000-0000-000000000001', 'access content'),
       ('00000000-0000-0000-0000-000000000001', 'view netgrasp devices')
ON CONFLICT DO NOTHING;
```

## The one architectural rule

**This repository builds on Trovato. It never modifies Trovato.**

Everything netgrasp-specific lives here: the plugin, the `netgrasp-core` crate,
the migrations, the manifest, the templates, the stylesheet, the script and the
install tooling. Trovato is consumed two ways and no others:

- at build time, as `trovato-sdk`, a git dependency pinned by revision;
- at run time, by appending three directories to Trovato's search paths.

There is no third way. No file here is copied into a Trovato checkout, no Trovato
source file mentions netgrasp, and installing this needs no Trovato patch. If a
feature here ever seems to require one, that is a bug in the feature.

## Status

Version 1.2.0, paired with
[netgraspd 1.2.0](https://github.com/jeremyandrews/netgraspd/releases/tag/v1.2.0);
see [Compatibility](#compatibility). `CHANGELOG.md` has the history and the
versioning rule: from 1.0.0 the plugin's version is its own, and the Trovato it
is built against is recorded beside it rather than being it.

Extracted from the Trovato monorepo via `git filter-repo`, preserving the
per-commit history of every netgrasp file.

**Builds standalone.** The plugin compiles to WebAssembly and the suite passes
with no Trovato checkout anywhere on disk. CI proves it: the `standalone` job
clones only this repository.

## Repository layout

```
plugins/netgrasp/          the plugin: taps, manifest, migrations, design notes
crates/netgrasp-core/      host-agnostic sync, write-back and timeline logic
templates/gather/          the nine page templates and four shared partials
  netgrasp/                  page chrome, device table, event table, summary
static/css/netgrasp.css    one stylesheet for every page
static/js/netgrasp.js      local timestamps and the auto-reload
scripts/
  build-overlay.sh           build the wasm, assemble overlay/plugins/netgrasp
  check-host-imports.sh      manifest capabilities vs the artifact's imports
  serve-demo.sh              install onto a Trovato checkout and serve
  first-run.sh               walk Trovato's installer wizard, for either path
  seed-demo.sql              a small home network, for pages with real rows
docker-compose.demo.yml    the one-command demo, on the published kernel image
docker/overlay.Dockerfile    assembles the overlay in a container, no host Rust
```

## Building

```bash
git clone git@github.com:jeremyandrews/netgrasp-trovato.git
cd netgrasp-trovato
cargo build --target wasm32-wasip1 --release
cargo test --workspace          # needs Postgres; see below
```

No Trovato checkout is required. `trovato-sdk` comes from the **public** Trovato
repository as a git dependency pinned by revision in the workspace `Cargo.toml`,
so unlike the Ritrovo precedent there is no private-repository blocker here.

The Rust toolchain and the `wasm32-wasip1` target are pinned in
`rust-toolchain.toml` and installed by rustup automatically.

`cargo test` needs a reachable Postgres for `crates/netgrasp-core`'s
daemon-schema test, which applies the daemon's own DDL to a scratch schema and
runs the plugin's real statements against it. It reads `DATABASE_URL`, defaulting
to `postgres://trovato:trovato@localhost:5432/trovato`. The other 123 tests need
nothing.

### Which Trovato revision this builds against

The pin is a commit, not a branch, and it is authored in one place:
`kernel-release.toml` at the repository root. Everything below is generated from
it by `scripts/sync-kernel-release.sh`, and `cargo test` fails by name if any
copy drifts.

<!-- kernel-release:begin -->
| | |
|---|---|
| pinned `rev` | `81290cc862b2c8302bc0a99d270f2571405c51ef` |
| Trovato version | 0.107.0 |
| `KERNEL_API_VERSION` | (0, 107) |
| manifest `api_version` | `0.107` |
| demo kernel image | `ghcr.io/jeremyandrews/trovato:nightly-81290cc@sha256:fca4070e75885aa91fa8cd9bae4e79d492e7ded2b7f86c2f7a5c3383392f729b` |
<!-- kernel-release:end -->

`api_version` is not chosen: it is the pinned version's major and minor, so it
cannot disagree with the kernel it is built against. The plugin's own `version`
is separate and is not the kernel's. To move the pin to a release, run
`scripts/sync-kernel-release.sh --set-version X.Y.Z`, which resolves the tag to
its commit and rewrites every location. Between releases,
`scripts/sync-kernel-release.sh --set-commit <sha>` pins a commit on Trovato's
`main` and the `nightly-<short sha>` image by digest, and refuses an image whose
build provenance names any other commit.

### And which Trovato release it runs on

A different question, with a different answer. The kernel's compatibility rule
(`PluginInfo::check_api_compatibility`) is **plugin major equal, plugin minor at
or below the kernel's**, so the module loads on its own kernel minor and on any
later `0.x`. The demo runs the published image named in the table above, and a
test pins that pair so bumping the image cannot quietly outrun what the manifest
claims.

What a newer kernel does *not* buy the plugin is host functions that did not
exist when it was built. Nothing here needs one, which is why the demo needs no
`api_version` bump and no rebuild against a newer SDK.

## Compatibility

This plugin is the web interface for
[netgraspd](https://github.com/jeremyandrews/netgraspd), and the two share one
database. They are separate releases that have to agree on one thing, the `ng_`
tables.

| netgrasp-trovato | netgraspd | `ng_` schema | Trovato kernel |
|---|---|---|---|
| 1.0.0 | 1.0.0 | version 3 (`V3__enrichment_location_people.sql` in netgraspd) | 0.102.0 or a later 0.x |
| 1.1.0 | 1.1.0 | version 5 (`V5__people_epochs_event_person_daemon_status.sql` in netgraspd); reads version 4's notify tables and none of version 5's yet | 0.102.0 or a later 0.x |
| 1.2.0 | 1.2.0 | version 7 (`V7__presence_opt_in.sql` in netgraspd) | 0.102.0 or a later 0.x |

**The rule.** A plugin and a daemon pair when they are built for the same `ng_`
schema version, which is the highest migration in the daemon's `migrations/`.
The daemon owns that schema and its migrations are the canonical DDL. This
repository carries the tables it reads twice: `crates/netgrasp-core/tests/
fixtures/daemon_schema.sql` reproduces the daemon's DDL and is what the plugin's
queries are tested against, and `plugins/netgrasp/migrations/001_netgrasp_schema.sql`
(with `015_netgrasp_notify_schema.sql` for version 4's notify tables and
`022_netgrasp_presence_schema.sql` for the columns versions 5 and 7 added)
is a guarded copy so the plugin can be enabled with no daemon. The fixture is
the daemon's schema as of version 7.

The next plugin release pairs with the netgraspd release that carries schema
version 7 (`V7__presence_opt_in.sql`): presence is opt in, and the plugin's
presence settings write the two columns that version adds.
`the_plugin_migration_is_a_faithful_copy_of_the_daemons_schema` checks the
second against the first column by column, and CI fails if it does not run.

**A schema change is a paired release.** Any change to an `ng_` table ships as a
new daemon release and a new plugin release together, each naming the other in
this table, and the daemon is upgraded and migrated first: `netgraspd migrate`
against an empty database, then this plugin. A release of either that does not
touch the schema pairs with the other side's current release, and says so here.

## Deploying: the kernel's own plugins

The four-entry menu needs nothing from the deployment. What the deployment
decides is whether the kernel plugins behind the old entries stay enabled at
all: a network monitor uses none of them, and netgrasp does not switch off
another plugin, because a site that has a blog would lose it. On a netgrasp-only
site, add these to the container's startup command, before `serve`. Each takes
one name, writes `plugin_status`, and takes effect at the next start:

```bash
./trovato plugin disable trovato_blog
./trovato plugin disable trovato_media
./trovato plugin disable trovato_categories
./trovato plugin disable trovato_content_translation
```

Keep `trovato_redirects` enabled: `/who-is-home` and the `/gather/<id>` paths
answer with its 301s. Verified on the demo stack: with the four disabled every
netgrasp page still renders and `scripts/verify-demo.sh` passes.

## Installing onto a stock Trovato

### From a release

Each [release](https://github.com/jeremyandrews/netgrasp-trovato/releases)
attaches `netgrasp-trovato-<version>.tar.gz` and its `.sha256`: the built module
with its manifest and migrations, the templates and the static files, laid out
as the three directories Trovato's search paths take. No Rust needed.

```bash
v=1.2.0
base=https://github.com/jeremyandrews/netgrasp-trovato/releases/download/v$v
curl -fLO "$base/netgrasp-trovato-$v.tar.gz"
curl -fLO "$base/netgrasp-trovato-$v.tar.gz.sha256"
sha256sum --check "netgrasp-trovato-$v.tar.gz.sha256"
tar -xzf "netgrasp-trovato-$v.tar.gz"
NG="$PWD/netgrasp-trovato-$v"

export PLUGINS_DIR="$TROVATO/plugins:$NG/plugins"
export TEMPLATES_DIR="$TROVATO/templates:$NG/templates"
export STATIC_DIR="$TROVATO/static:$NG/static"
```

Then install and serve as in steps 3 to 5 below.

#### Onto the published kernel image, beside the daemon

`docker-compose.demo.yml` is not this case: it builds the module from source,
bind-mounts `templates/` and `static/` from the clone, and brings its own
Postgres with demo rows. With the tarball extracted to `$NG` as above, a daemon
that has already run `netgraspd migrate` on its database, and no Rust:

```bash
img=ghcr.io/jeremyandrews/trovato:0.102.0   # or any later 0.x
run() {
  docker run --rm --network host \
    -e DATABASE_URL=postgres://ROLE:PASSWORD@127.0.0.1:5432/netgrasp \
    -e REDIS_URL=redis://127.0.0.1:6379 \
    -e PLUGINS_DIR=/app/plugins:/netgrasp/plugins \
    -e TEMPLATES_DIR=/app/templates:/netgrasp/templates \
    -e STATIC_DIR=/app/static:/netgrasp/static \
    -v "$NG/plugins:/netgrasp/plugins:ro" \
    -v "$NG/templates:/netgrasp/templates:ro" \
    -v "$NG/static:/netgrasp/static:ro" \
    "$@"
}
run "$img" ./trovato plugin install netgrasp   # migrations, then enable
run -d -e PORT=3000 "$img" ./trovato serve
scripts/first-run.sh http://127.0.0.1:3000     # a brand new database only (step 5)
```

`podman run` takes the same arguments. Under Kubernetes the image in the next
section does the copying, and the variables are the same.

**Which database role.** The kernel shares the daemon's database, so
`DATABASE_URL` names a role there. Two ways work:

- **The daemon's own role**, the one that owns the `ng_` tables. Nothing to
  grant. It is the simplest, and the kernel can then write any daemon column,
  which the plugin's code is the only thing preventing.
- **A role of the kernel's own**, which needs `CREATE` on the schema (for the
  kernel's tables and the plugin's own `ng_state` and `ng_person_merge`) and
  these on the daemon's tables, run as their owner after every
  `netgraspd migrate` so tables a new daemon version adds are covered:

  ```sql
  -- every ng_ table and view; run as their owner, this reaches only the
  -- tables that role owns, and Postgres warns about the rest
  GRANT SELECT ON ALL TABLES IN SCHEMA public TO trovato;
  -- the admin's edits, the Item link and the dirty flag the cron sweep lowers
  GRANT UPDATE (display_name, notes, hidden, notify, owner_item_id,
                counts_for_presence, trovato_item_id, sync_state)
        ON ng_devices TO trovato;
  -- the person mirror: an upsert on item_id, and a delete when a person goes
  GRANT INSERT (item_id, name, notes, notify_arrive, notify_depart, track_presence),
        UPDATE (item_id, name, notes, notify_arrive, notify_depart, track_presence),
        DELETE ON ng_people TO trovato;
  -- the cron sweep prunes expired events
  GRANT DELETE ON ng_events TO trovato;
  -- Send test on /alerts
  GRANT INSERT (requested_by) ON ng_notify_requests TO trovato;
  -- one person per name (plugin migration 018 creates this only as the owner)
  CREATE UNIQUE INDEX IF NOT EXISTS ng_people_name_key ON ng_people (lower(btrim(name)));
  ```

  The plugin's migrations are written for this: wherever they touch a daemon
  table they create nothing on it unless the role owns it, and a test applies
  them as a role that owns no daemon table.

**The expected warning.** `plugin install` from the tarball prints

```
Warning: WASM not found at '/netgrasp/target/wasm32-wasip1/release/netgrasp.wasm'. Build it first with:
  cargo build --target wasm32-wasip1 -p netgrasp --release
```

It is harmless and needs no Rust. `plugin install` offers to copy a fresh build
over the module in the plugin directory, looking for one under a `target/` next
to it; a release has none, so it carries on with the tarball's `netgrasp.wasm`,
runs the migrations and enables the plugin.

### From the image

Every push to `main` publishes `ghcr.io/jeremyandrews/netgrasp-trovato:sha-<12
character commit>`, and a release tag publishes `:<version>` and `:latest`, for
`linux/arm64` and `linux/amd64`. The image holds the same three directories as
the tarball at `/netgrasp/plugins`, `/netgrasp/templates` and `/netgrasp/static`,
on busybox so it can copy itself: run it as an init container with
`cp -a /netgrasp/. /shared/`, mount the shared volume read-only at `/netgrasp` in
the published kernel image, and set `PLUGINS_DIR=/app/plugins:/netgrasp/plugins`,
`TEMPLATES_DIR=/app/templates:/netgrasp/templates` and
`STATIC_DIR=/app/static:/netgrasp/static`. Pin a `sha-` tag or a version, never
`latest`, so the plugin cannot move underneath a deployment. It is built from
`docker/overlay.Dockerfile`'s `published` stage by `.github/workflows/image.yml`.

### From source

Trovato reads `PLUGINS_DIR`, `TEMPLATES_DIR` and `STATIC_DIR` as
colon-separated **search paths**, and a later entry wins on a name collision.
That is the whole integration seam.

```bash
# 1. Build the plugin and assemble the directory a deployment consumes.
scripts/build-overlay.sh

# 2. Point a stock Trovato at it. Nothing is copied into the Trovato checkout.
export PLUGINS_DIR="$TROVATO/plugins:$PWD/overlay/plugins"
export TEMPLATES_DIR="$TROVATO/templates:$PWD/templates"
export STATIC_DIR="$TROVATO/static:$PWD/static"
export DATABASE_URL=postgres://trovato:trovato@localhost:5432/netgrasp

# 3. Install. Runs the migrations and enables the plugin.
$TROVATO/target/release/trovato plugin install netgrasp

# 4. Serve.
$TROVATO/target/release/trovato serve

# 5. On a brand new database, walk Trovato's own first-run wizard. Until it is
#    done the kernel answers every path except /health, /static and /install
#    with a redirect to /install, so every netgrasp page is an installer form.
scripts/first-run.sh http://localhost:3101
```

`scripts/serve-demo.sh <path-to-trovato> [--seed] [--bg]` does all of that,
including the startup a brand new database needs between installing the plugin
and seeding it (the kernel registers the plugin's Item types then, and `item.type`
is a foreign key onto `item_type`), and `--seed` loads demo rows so the pages
have something on them.

Trovato used to ship an in-tree copy of this plugin at `plugins/netgrasp`, and
the search path made that harmless: the kernel logged `plugin name found in more
than one plugins directory; the later directory on the search path wins`, naming
both. That is over. Trovato removed its copy when netgrasp was extracted, so
neither the source tree nor the published image carries one, and the overlay is
the only netgrasp the kernel discovers. Verified against the
published kernel image this repository pins (`0.102.0`), whose `/app/plugins`
holds 39 entries, 37 of them plugins with a manifest, and no `netgrasp`.

The precedence still matters for the other two paths, and it is still the reason
they are search paths: `TEMPLATES_DIR` is how a netgrasp template could override
a base one by name, and it is what lets the nine page templates be found at all
without being copied into the image.

## The pages

| URL | What it is |
|---|---|
| `/` | redirects to `/overview` |
| `/overview` | **Home** in the menu: who is home and since when, who is away, today's arrivals and departures, then what needs attention (spoofing, rogue DHCP or an address conflict in the last day, up to six new devices nobody has sorted, each with an Assign link, and a count of the rest), then how many devices are online. On a phone, Needs attention moves up under Home now |
| `/devices` | **Devices** in the menu: what is on the network right now (the Online tab) |
| `/devices/all` | every device the daemon has seen, except the hidden ones |
| `/devices/hidden` | the devices somebody hid, which no other list shows |
| `?search=…` | on every Devices tab: any name, the MAC, the vendor, the IP or the owner, case-insensitive; blank shows everything |
| `/activity` | **Activity** in the menu: arrivals, departures, new devices, and the three security events worth acting on (spoofing, rogue DHCP, address conflicts) |
| `/people/home` | the people the daemon counts as home, with the time each arrived |
| `/people/movements` | every arrival and departure; `?day=YYYY-MM-DD` for one day |
| `/devices/new` | devices first seen in the last seven days, with the daemon's fingerprint and how sure it is |
| `/devices/todo` | devices nobody has named or given an owner: what is left to do after a new device appears |
| `/events/new-devices` | every `new_device` event the daemon recorded |
| `/devices/online` | what is on the network right now; the same page as `/devices` |
| `/devices/location` | devices grouped by where the daemon's UniFi enrichment placed them; empty, and saying why, when enrichment is off |
| `/devices/type` | every device, one section per type; `?device_type=…` for one type, reached by clicking a Type cell |
| `/devices/owner?owner=…` | one person's devices; reached by clicking an Owner chip |
| `/who-is-home` | a 301 to `/people/home` |
| `/people` | **People** in the menu: people devices can belong to |
| `/events` | everything the daemon noticed, security rows flagged in place |
| `/events/security` | scans, spoofs, rogue DHCP, conflicts, identity changes |
| `/events/device?device=…` | one device's events; reached by clicking a Device chip |

The navigation is four entries: **Home, Devices, People, Activity**
(`docs/UI.md`, "Navigation"). Every other page is a tab on one of them or is
reached from a row. `011_netgrasp_navigation.sql` writes them as `menu_link`
rows, and the kernel draws `menu_link` rows instead of every plugin's `tap_menu`
entries whenever the site has any, so they are the whole site menu from the
next request: the kernel's own Home, Translate, Post, Blog, Terms, Translate
Language and Media entries leave the bar with no restart. The plugin's
`tap_menu` declares the same four as the fallback for a site that empties
`menu_link`. The rows are editable at `/admin/structure/menus`, and a second run
of the migration leaves an edited row alone.

The kernel draws no second menu level, so a section's pages are tabs drawn by
the page itself (`templates/gather/netgrasp/tabs.html`). Activity's tabs are
Notable, Comings and goings, New devices, Security and Everything. Devices'
are Online, All, New, To sort, Hidden, By place and By type, each with the same
search box: a plain GET form, so it works with scripting off. Below 640 px
every table is a stack of cards (name and row menu, then owner, state and
time), so nothing scrolls sideways on a phone.

Every old path still answers. `/devices` now opens on what is online and the
full list is `/devices/all`; `/who-is-home` is a real 301 to `/people/home`,
from the `redirect` table, which the `trovato_redirects` plugin serves (it is on
by default; keep it on).

`/` lands on the online devices because the same migration sets
`site_front_page`, which Trovato serves as a redirect for any internal path. It
uses `ON CONFLICT DO NOTHING`, so an operator's own choice is never overwritten.

### The row menu

Every device row, every event row and every person card carries an actions menu,
opened with the `⋯` button at the end of the row or by right-clicking anywhere in
it.

| Entry | On | What it opens |
|---|---|---|
| Rename | devices, people | a one-field form for the name |
| Assign owner | devices | a select of people, plus "nobody" |
| Hide / Unhide | devices | a confirmation, labelled with the direction it is going |
| Mute / Unmute alerts | devices | the same, for arrival and departure alerts |
| Arrival alerts | people | that person's two notification flags |
| Ask the assistant about this | everything | a conversation scoped to the row |

**It is a menu that works with scripting off.** The menu itself is a `<details>`
element, so the browser opens it on click and on Enter or Space with no script
involved, and every entry in it is an ordinary link. The right-click handler in
`static/js/netgrasp.js` opens that same element under the pointer and closes any
other; Escape closes it and puts focus back on the button. Touch gets the visible
button, which is why the button is visible rather than appearing on hover.

Each entry links to a small page the plugin serves rather than posting straight
from the row, and that is forced rather than chosen: a write has to carry the
kernel's `_token`, and a gather content template is rendered in a context that
does not contain one — the same context limitation the auto-reload section below
describes, met from a second direction. The page the link opens is served by
`tap_api`, which *is* handed a token, so it renders the form and the POST goes
from there. `plugins/netgrasp/FRICTION.md`, `G-GATHER-TEMPLATE-NO-CSRF`.

An event row's menu acts on the **device** the event is about, because an event
has nothing of its own to configure. It reaches that device by the `device_id` on
the row, since a gather reads one record type and the MAC is not on it.

#### The pages behind it

| Route | Methods | What it does |
|---|---|---|
| `/netgrasp/device/rename` | GET, POST | the device's display name |
| `/netgrasp/device/owner` | GET, POST | a select of people, plus "nobody" |
| `/netgrasp/device/hidden` | GET, POST | hide or unhide, whichever it is not |
| `/netgrasp/device/notify` | GET, POST | mute or unmute its arrival alerts |
| `/netgrasp/person/rename` | GET, POST | the person's name |
| `/netgrasp/person/notify` | GET, POST | their two notification flags |

Every one is registered by `tap_menu` as an invisible `api` entry gated on
`administer netgrasp` and served by `tap_api`. The `GET` renders one field with
the kernel's `_token` in a hidden input; the `POST` writes and comes back. A
device is named by its MAC **or** by its `ng_devices` row id, which is how an
event row reaches the same forms.

**The writes go through the assistant's own code**, not beside it —
`apply_device_edit` and `apply_person_save`, the same functions the assistant's
tools call. A device renamed from a menu and a device renamed in a conversation
mint the Item the same way, write the same single column, and leave `sync_state`
alone the same way, because they are one function called twice. The edit is
sparse: renaming a device names `display_name` and nothing else, which is what
stops a rename from turning the device's alerts off.

Two things these pages cannot do, both recorded in `FRICTION.md`. They cannot
redirect — `ApiResponse` has no headers, so there is no `Location` to send, and a
confirmation with a `<meta refresh>` is the closest no-JavaScript equivalent. And
the permission is checked twice by two checks that disagree: the kernel's gate
honours `administer site`, the plugin's host call does not, so a site that wants
somebody using this grants `administer netgrasp` for real.

### Auto-reload

The device, event and presence pages reload themselves. **Ten seconds** by
default, `?refresh=<seconds>` overrides it for one page load, and `0` disables it
with no timer armed at all.

The default is one line in `templates/gather/netgrasp/page.html`, marked as the
knob, and that file explains at length why it lives there rather than in site
configuration: a gather content template is rendered in its own context, and the
site context that would carry a setting is injected into the page template around
it. Making it a site setting would mean changing Trovato, which the rule above
forbids for something netgrasp can answer itself.

Only netgrasp's own pages carry the timer. `static/js/netgrasp.js` returns
immediately unless it finds an `.ng-page` element, so nothing else on a host site
reloads.

The reload **defers** while the page is in use — a menu standing open, or focus
in a field — and arms itself again rather than cancelling. Reloading a page ten
seconds after somebody opened a menu would close it before they had read it;
cancelling outright would leave a wall display frozen because somebody walked
past and opened one.

The same script rewrites every timestamp into the viewer's timezone. The server
renders UTC from the daemon's `_epoch` columns and the browser, which knows where
it is, corrects it; the UTC stays in the cell's `title` and is the fallback with
scripting off.

## Verifying a change

- `cargo test --workspace` — over 300 tests, including drift checks that tie the
  templates, the manifest, the migrations and the demo compose file to each
  other. Several exist because
  a Tera render that reaches for an undefined variable does not warn: it aborts,
  and the route falls back to dumping every column of the base table. Those tests
  are what notice — and one of them now *renders* all three listing templates
  with the rows they really get, nulls included, rather than searching them for
  strings. A string search cannot tell a working template from one that raises.
- `scripts/check-host-imports.sh` — the manifest's declared capabilities against
  the compiled module's actual imports, in both directions.
- `docker compose -f docker-compose.demo.yml up --build` then load the pages.
  Row counts are checkable: `scripts/seed-demo.sql` prints its own counts per
  listing when it finishes, and every listing above is a `count(*)` you can hold
  a page against. The `installer` container fails the run if `/overview`
  answers anything but 200, and `scripts/verify-demo.sh` then checks that `/`
  redirects there and that the page is Home's own template rather than the
  kernel's column dump, that a visitor who is not signed in gets no MAC address
  and a sign-in line, and then, signed in (`ADMIN_USER` and `ADMIN_PASSWORD`,
  the demo's by default), the menu, every old path, the redirects, the tabs, the
  search, and Home's sections.
- `scripts/serve-demo.sh <trovato> --seed --bg` for the same thing against a
  Trovato source checkout, which is the path to use when the kernel is what you
  are changing.

## Not here

The daemon is a separate repository, [netgraspd](https://github.com/jeremyandrews/netgraspd).
New-device notifications are the daemon's (it emits `new_device` events and can
notify over ntfy); surfacing and routing them on the web side is not built yet.

## License

MIT.
