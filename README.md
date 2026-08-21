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

Then open **http://localhost:3101/**, which redirects to `/devices/online`. The
[six-entry navigation](#the-pages) reaches every page from there.

The first run compiles the plugin inside a container and pulls the published
kernel image, so it takes a few minutes. After that it is seconds. What comes up:

| | |
|---|---|
| kernel | `ghcr.io/jeremyandrews/trovato:0.101.0`, the published image, unmodified |
| netgrasp | the plugin, the templates and the assets, mounted read-only on the three search paths |
| rows | `scripts/seed-demo.sql`: 13 devices, 3 people, 19 events, 6 of them security |
| clock | a container POSTing `/cron/<CRON_KEY>` every 60 seconds, because the kernel runs no scheduler |

Nothing is copied into Trovato and nothing is written to your clone: the
assembled plugin directory lives in a Docker volume, and `templates/` and
`static/` are mounted from the repository read-only.

`docker compose -f docker-compose.demo.yml down -v` removes all of it, volumes
included. `up --build` rebuilds the plugin after an edit. The compose file
documents the ordering a brand new database needs, and the one expected warning
on first run.

### Putting it behind a login

The demo assumes a localhost dashboard, so the navigation and every page are
visible to an anonymous viewer. That is one line:
`plugins/netgrasp/migrations/005_netgrasp_web_interface.sql` grants
`view netgrasp devices` to the anonymous role. Delete that `INSERT` and the pages
need a login, with no other change: the `network_viewer` role already holds the
same permission, so an authenticated viewer keeps the whole navigation.

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

The pin is a commit, not a branch. The triple that moves together, recorded in
the workspace `Cargo.toml` next to the dependency:

| | |
|---|---|
| pinned `rev` | `611c1fb72a60cb8528b93db2c6ab40aa564bee39` |
| Trovato version | 0.99.0 |
| `KERNEL_API_VERSION` | (0, 99) |

`api_version` in `plugins/netgrasp/netgrasp.info.toml` tracks that kernel API
version, and a test asserts the pair has not drifted. The bump protocol is in the
`Cargo.toml` comment.

### And which Trovato release it runs on

A different question, with a different answer. The kernel's compatibility rule
(`PluginInfo::check_api_compatibility`) is **plugin major equal, plugin minor at
or below the kernel's**, so a module declaring `0.99` loads on any `0.x` kernel
from 0.99 up. The demo runs the published `0.101.0` image with this manifest
unchanged, and a test pins that pair so bumping the image cannot quietly outrun
what the manifest claims.

What a newer kernel does *not* buy the plugin is host functions that did not
exist when it was built. Nothing here needs one, which is why the demo needs no
`api_version` bump and no rebuild against a newer SDK.

## Installing onto a stock Trovato

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

# 3. Install. Runs the five migrations and enables the plugin.
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
the only netgrasp the kernel discovers. Verified against
`ghcr.io/jeremyandrews/trovato:0.101.0`, whose `/app/plugins` holds 38 plugin
directories and no `netgrasp`.

The precedence still matters for the other two paths, and it is still the reason
they are search paths: `TEMPLATES_DIR` is how a netgrasp template could override
a base one by name, and it is what lets the nine page templates be found at all
without being copied into the image.

## The pages

| URL | What it is |
|---|---|
| `/` | redirects to `/devices/online` |
| `/devices/online` | what is on the network right now |
| `/devices` | every device the daemon has seen |
| `/devices/type?device_type=…` | one device type; reached by clicking a Type cell |
| `/devices/owner?owner=…` | one person's devices; reached by clicking an Owner chip |
| `/who-is-home` | online devices grouped by the person who owns them |
| `/people` | people devices can belong to |
| `/events` | everything the daemon noticed, security rows flagged in place |
| `/events/security` | scans, spoofs, rogue DHCP, conflicts, identity changes |
| `/events/device?device=…` | one device's events; reached by clicking a Device chip |

The navigation is six `tap_menu` entries the kernel renders as the site menu.
They appear because `005_netgrasp_web_interface.sql` grants
`view netgrasp devices` to the anonymous role — the assumption being a localhost
dashboard with no login. To put the whole thing behind a login, delete that one
`INSERT`: the `network_viewer` role already holds the same permission, so an
authenticated viewer keeps the navigation with no further change.

`/` lands on the online devices because the same migration sets
`site_front_page`, which Trovato serves as a redirect for any internal path. It
uses `ON CONFLICT DO NOTHING`, so an operator's own choice is never overwritten.

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

The same script rewrites every timestamp into the viewer's timezone. The server
renders UTC from the daemon's `_epoch` columns and the browser, which knows where
it is, corrects it; the UTC stays in the cell's `title` and is the fallback with
scripting off.

## Verifying a change

- `cargo test --workspace` — 170 tests, including drift checks that tie the
  templates, the manifest, the migrations and the demo compose file to each
  other. Several exist because
  a Tera render that reaches for an undefined variable does not warn: it aborts,
  and the route falls back to dumping every column of the base table. Those tests
  are what notice.
- `scripts/check-host-imports.sh` — the manifest's declared capabilities against
  the compiled module's actual imports, in both directions.
- `docker compose -f docker-compose.demo.yml up --build` then load the pages.
  Row counts are checkable: `scripts/seed-demo.sql` prints its own counts per
  listing when it finishes, and every listing above is a `count(*)` you can hold
  a page against. The `installer` container fails the run if `/devices/online`
  answers anything but 200.
- `scripts/serve-demo.sh <trovato> --seed --bg` for the same thing against a
  Trovato source checkout, which is the path to use when the kernel is what you
  are changing.

## Not here

The daemon is a separate repository, [netgraspd](https://github.com/jeremyandrews/netgraspd).
New-device notifications are the daemon's (it emits `new_device` events and can
notify over ntfy); surfacing and routing them on the web side is not built yet.

## License

MIT.
