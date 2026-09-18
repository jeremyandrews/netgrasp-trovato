# The first joint run: daemon, plugin, one database, assistant on

2026-09-17, on a MacBook Pro against a live home LAN (~48 devices). The daemon
and this plugin had never been run together: the shared schema was asserted from
each side against the DDL both repositories agree on, and nothing more. This is
the record of running them together, with the AI assistant enabled and a real
model answering.

Nothing in either repository was changed to make it work. Every defect below is
a finding, not a fix.

## What ran

| Piece | Version |
|---|---|
| `netgraspd` | `main` at `ecb99e7`, version 0.1.0, built locally, release profile |
| `netgrasp-trovato` | `main` at `c4dd55a`, manifest `api_version = "0.102"` |
| Trovato kernel | `ghcr.io/jeremyandrews/trovato:0.102.0`, built from `20baa12` |
| `trovato-sdk` pin | `rev = ca76a96`, five commits before `20baa12` |
| Postgres | `pgvector/pgvector:pg17` |
| Model | `claude-sonnet-4-6` (see the Sonnet 5 finding) |

**The pin gap is not a problem.** The plugin's wasm was built against SDK
`ca76a96` and ran inside a kernel built from `20baa12`. The five commits between
them (PRs 61, 62, 64, 65, 68) touch nothing in the plugin contract, and the
kernel loaded the plugin, registered its Item types, ran its taps and dispatched
its assistant tools without a single version complaint.

The daemon captured on `en0` as root (macOS has no `setcap`; BPF is root-only on
this machine and no ChmodBPF helper is installed). There is no replay or fixture
mode in the binary: `pcap::Capture::from_device` (`src/capture/mod.rs:178`) is
the only frame source, and the 23 recorded frames in `tests/fixtures/` are
compiled into the test binary with `include_bytes!`. Live capture was the only
way to run this.

## The order a fresh database needs

The plugin's demo compose was used with a small override (below), under a
separate compose project so an existing demo stack was left alone.

1. **The daemon migrated first.** `netgraspd run` as an unprivileged user
   applied V1, V2 and V3, passed its own schema preflight, and then stopped at
   the BPF permission error. That is a clean way to migrate without `sudo`:
   `Db::migrate` runs at `daemon.rs:117`, and the capture permission check is
   forty lines later.
2. **The plugin migrated second.** `trovato plugin install netgrasp` applied
   `001` through `006` over the daemon's tables. No error, no warning: the
   plugin declares the shared tables with `CREATE TABLE IF NOT EXISTS`, so it
   adopted them.
3. **The daemon still agreed afterwards.** `netgraspd stats` passed
   `require_schema` and listed all eight `ng_` tables once the plugin's
   migrations had run.

**Finding: the daemon's own compose file cannot migrate a fresh database.**
`docker-compose.yml` runs `maintain --dry-run` as its one-shot `migrate`
service, with a comment calling it "the smallest command that proves the schema
is right". It does not migrate. On an empty database it exits 1 with "this
database has no netgrasp schema", and its own error message says to run
`netgraspd run` once, "(docker-compose.yml encodes the same thing as a one-shot
`migrate` service)", which is exactly what it does not do. Confirmed twice: on
the joint database before anything else ran, and on a scratch database created
and dropped for the purpose. Every service in that file waits on `migrate`
completing successfully, so `docker compose up` on a new install never starts
the daemon.

**Sync works.** Every daemon row got an `ng_device` Item
(`sync: 1 created, 0 relinked, 0 refreshed, 16 skipped, 0 failed`), 48 rows and
48 Items at the end of the run, none unlinked. Rows return to `dirty` constantly
because every daemon write sets `sync_state = 'dirty'` and the daemon flushes
`last_seen_at` on a timer, so each cron tick re-examines every active device and
plans `Skip` for it. That is the designed behaviour, not a leak: `sync_one`
clears the row whichever action it plans.

## Configuration used

The daemon (`netgrasp.toml`, thresholds shortened so presence changes happen in
minutes rather than hours):

```toml
[database]
url = "postgres://trovato:trovato@127.0.0.1:55432/netgrasp"

[capture]
interfaces = ["en0"]
sources = ["arp", "mdns", "dhcp", "ssdp"]

[state]
idle_timeout = "2m"
offline_timeout = "4m"
flush_interval = "10s"
sweep_interval = "10s"

[learning]
on_first_run = true
duration = "1m"

[notify]
enabled = true
debounce = "10s"
batch_threshold = 0
batch_window = "1s"
event_types = ["new_device", "returned", "went_offline", "name_updated"]

[notify.ntfy]
topic = "joint-run"
server = "http://127.0.0.1:18080"   # a local request logger, so nothing leaves the machine
priority = 3
```

The compose override, applied on top of `docker-compose.demo.yml` and kept
outside the repository:

```yaml
services:
  postgres:
    ports:
      - "127.0.0.1:55432:5432"      # the daemon runs on the host, so the database must be reachable there
  trovato:
    environment:
      ANTHROPIC_API_KEY: ${ANTHROPIC_API_KEY:?}   # name only; the value comes from the invoking shell
  seed:
    command: ["true"]               # no synthetic rows: every device here came from the daemon
```

The API key was passed to `docker compose` from the environment and never
written to a file in either repository.

## The assistant

Configured through the admin UI at `/admin/system/ai-providers` and
`/admin/system/ai-assistant`: an Anthropic provider with
`base_url = https://api.anthropic.com/v1` and `api_key_env = ANTHROPIC_API_KEY`,
set as the default for chat, assistant enabled, all three netgrasp scopes on,
every limit left at its default.

Conversations were held as a user named `netadmin` whose only role is the
plugin's `network_admin`, with `is_admin` false. That is the real grant
`administer netgrasp`, not `administer site`, which is what
`G-USER-API-NO-ADMIN-BYPASS` in `FRICTION.md` says the tools require.

### Scope exercises

Every write was dispatched `Describe` first and produced a proposal card. In
every case the database was byte-identical before Apply, checked by diffing the
device row and its Item.

| # | Scope | Asked | Card | Unchanged before Apply | After Apply | Wall | Tokens |
|---|---|---|---|---|---|---|---|
| 1 | device | "Rename this device to Office printer." | `rename`, low risk, names the MAC and both names | yes | Item title, `display_name`, and `netgraspd devices` all read "Office printer" | 11.7s | 3,865 |
| 2 | network | "Create a person named Jamie. Notify me when Jamie arrives, not when Jamie leaves." | `create_person`; the model said it needed the new id before setting flags | yes | `ng_person` Item plus the `ng_people` mirror row | 12.0s | 7,498 |
| 3 | network | "Applied. Now set Jamie to notify on arrival only." | `set_person_notify` | yes | `notify_arrive = t`, `notify_depart = f` on Item and mirror | 4.0s | 8,109 |
| 4 | device | "This device belongs to Jamie." | `set_owner`, after calling `list_people` itself | yes | `owner_item_id` and `field_owner` both set | 12.5s | 7,696 |
| 5 | device | "Mute alerts for this device." | `set_flags`, low risk | yes | applied, but `notify` was already false (see below) | 3.7s | 5,729 |
| 6 | network | "Who was online yesterday?" | read-only, `who_was_online` | n/a | 0 spans | 5.2s | 8,578 |
| 7 | network | "And who has been online in the last hour?" | read-only | n/a | 46 spans, grouped by owner, Jamie's device named | 7.7s | 11,684 |
| 8 | device | "This device belongs to Jamie, and mute its alerts." | two cards in one turn: `set_owner` + `set_flags` | yes | both applied | 7.2s | 8,170 |

Nine turns across five conversations, 61,329 tokens total by the kernel's own
accounting (each turn resends its history), seven proposals, all applied, none
discarded. Wall clock per turn 3.7s to 12.5s. At Sonnet 4.6 list prices that is
roughly twenty cents for the whole run.

The tool surface behaved: the model called `list_people` before assigning an
owner rather than guessing an id, split a two-part request into a create and a
follow-up when the create tool could not carry the flags, and put two cards in
one turn when both were expressible.

## Step 3: does the running daemon notice a change made through the assistant?

**CONFIRMED. It does not, until it restarts. After a restart, both the flag and
the owner land.**

The device: `92:27:e6:9f:03:76`, a private Wi-Fi address, `ng_devices.id = 31`.
Through the assistant, on the device page, it was assigned to Jamie and its
alerts muted; both cards applied at 16:34:58 UTC, leaving `notify = f` and
`owner_item_id` set to Jamie's Item.

**Before restarting** (daemon up since 15:34:37):

- 35 presence events for that device between 16:34:58 and 19:54.
- 33 of them were delivered (`ng_events.notified = t`), 17 `went_offline` and
  16 `returned`. The two undelivered ones were held by the 10s debounce, not by
  the mute. The notification sink received them all, titled with the daemon's
  own idea of the name.
- Zero `person_*` events in the whole database; Jamie stayed `away`.
- The same pattern on a second device, `9c:76:0e:30:aa:b0`, muted and assigned
  at about 15:59: deliveries continued at 16:00, 16:01, 16:04, 16:07 and
  16:11:49.

**After restarting** (19:59:48, `restored device table from Postgres devices=48
learning=false people=1`):

- `went_offline` at 20:01:38 recorded `notified = f`. Nothing was delivered.
- `returned` at 20:02:11 recorded `notified = f`. Nothing was delivered for the
  device.
- `person_arrived` for Jamie at the same instant recorded `notified = t`, and
  "Jamie arrived" reached the sink at 20:02:12. Jamie is now `home`.

So both halves of the change reached the daemon only through the restart, and
the mechanism is exactly the rehydrate block at the top of `daemon.rs::run`:
`queries::load_devices` and `build_registry` read the user-owned columns once,
at startup, into `Manager` and the people registry.

**The contract the daemon side owed holds.** `DESIGN.md` recorded that nobody
had verified the daemon tolerates another process writing the user-owned
columns. It does: `UPDATE_DEVICE_SQL` names none of them, and across three hours
of flushes the assistant's `display_name`, `notify` and `owner_item_id` survived
untouched.

## Findings

**Since this run:** the three plugin findings below are fixed, with regression
tests at the layer each one happened at; `CHANGELOG.md` has the root cause of
each. The seven kernel findings are reported and not patched — nothing in this
repository changes the kernel — and are now ledger items in
`plugins/netgrasp/FRICTION.md` rather than only a record of one run. The daemon
findings belong to the `netgraspd` repository and are untouched here.

The text below is left as it was written, as the record of what the run found.

### Plugin

1. **Any assistant device edit silently turns alerts off.**
   `apply_device_edit` rebuilds the whole `fields` map, and for a key the edit
   does not set it falls back to the Item
   (`plugins/netgrasp/src/assist_host.rs:291-293`), where `field_bool` maps a
   missing value to `false`. The cron sync creates device Items carrying only
   `field_mac`, so on any device whose Item has never been edited, the first
   rename or owner assignment writes `notify = false`. Observed twice: the
   rename in exercise 1 and the owner assignment in exercise 4 each flipped
   `notify` from true to false, and the proposal card said nothing about it. At
   the end of this run 33 of 34 device Items still had no `field_notify`, so the
   next edit of any of them would do the same. A muted device stays muted
   forever, because the same fallback re-reads what the previous edit wrote.
2. **Synced Item titles ignore the name the daemon resolved.**
   `derive_title` is `COALESCE(display_name, hostname, vendor || ' device', mac)`
   and skips `resolved_name` and `mdns_name`. The printer's Item was titled
   "CLOUD NETWORK TECHNOLOGY SINGAPORE PTE. LTD. device" while every page and
   the proposal card called it "Brother HL-L8360CDW series".
3. **The network scope's context does not say when monitoring began.** Asked
   who was online yesterday, the model correctly found zero spans and then
   speculated about "a gap in monitoring", because nothing told it the daemon's
   earliest observation was that morning.

### Kernel (0.102.0, reported, not patched)

4. **The assistant cannot use Claude Sonnet 5, Opus 5, Opus 4.7/4.8 or Fable.**
   `AssistantConfig.temperature` is a plain `f32` with no "unset", and
   `chat_complete` passes it on every call
   (`crates/kernel/src/services/ai_assistant.rs:794`), so
   `build_anthropic_request` always sends `temperature`. Those models reject it:
   sent directly, the identical request returns
   `400 invalid_request_error: "temperature is deprecated for this model."`, and
   without the field it returns 200. This is what blocked the run on
   `claude-sonnet-5`; `claude-sonnet-4-6` was used instead.
5. **The provider error body is discarded.** The kernel logs
   `assistant model call failed ... error=the AI provider returned HTTP 400` and
   the chat shows "Something went wrong talking to the model provider." The
   provider's own message, which names the offending field, is never recorded.
   Finding 4 took a direct reproduction to diagnose.
6. **Saving the permissions page deletes every plugin permission from every
   role.** `save_permissions` rebuilds each role's grants from
   `KERNEL_PERMISSIONS` alone, so `Role::set_permissions` removes everything
   else. One save removed 29 grants across seven roles, including
   `administer netgrasp` from `network_admin`, the anonymous role's
   `view netgrasp devices` (which the demo depends on), every `ng_device` and
   `ng_person` content grant, and the kernel's own `view own profile`, which is
   not in that list either. Restored here with SQL.
7. **There is no way to put a user in a role.** The admin user forms carry name,
   email, password, status and an `is_admin` checkbox, with no role selector;
   the role form carries only a name; `trovato user` has one subcommand,
   `reset-password`; `Role::assign_to_user` has no caller outside tests.
   `netadmin` was put in `network_admin` with SQL.
8. **The chat page reuses a single-use CSRF token.** `static/js/assistant.js`
   reads `data-csrf-token` once and sends it on every message, Apply and
   Discard, but `verify_csrf_token` consumes the token on success. Reproduced:
   the message succeeded, and Apply with the same token returned
   `403 Invalid or missing CSRF token` and changed nothing; with a token from a
   fresh page render it applied. In a browser, Apply on a card should fail until
   the page is reloaded. The form fallback posts the same consumed token.
9. **The stream's `done` event reports zero usage.**
   `{"type":"done","usage":{"completion_tokens":0,"prompt_tokens":0,"total_tokens":0}}`
   on every turn, while the same event's `tokens_used` and the conversation row
   carry the real figure.
10. **The provider connection test calls a 404 a success.** "Connected
    successfully (HTTP 404 Not Found)", latency 294ms, against a base URL that
    was in fact correct. Any reachable host would pass.

None of items 4 to 10 appear in `plugins/netgrasp/FRICTION.md`, which predates
this run.

### Daemon, on a real LAN

11. **mDNS names are attributed to the wrong device, constantly.** One MAC
    accumulated seven `mdns_name` signals, including five other hosts' names and,
    at one point, a neighbour's iPad. "Jeremy's MacBook Pro (2)" appeared on six
    different MACs. Device 31's own name, `Filbert-3`, was one signal among
    several. This drove 385 `name_updated` notifications in the first thirty
    minutes (that event type was enabled in this run's config, which is not the
    default; the flapping underneath it is not a config choice).
12. **The gateway triggers the security analyzers continuously.**
    `18:fd:74:39:e5:23` (Routerboard) was either the accused MAC or the
    conflicting holder in every one of 23 `ip_conflict` and 11 `arp_spoof`
    alerts in the first half hour, including for device 31's address. It is
    routing several VLANs and answering ARP for hosts on them. A real
    installation would need `security.exempt_macs` or a gateway-aware rule; as
    shipped, the alerts are continuous and indistinguishable from an attack.
13. **Notification titles are mojibake for non-ASCII names.** `X-Title` carries
    raw UTF-8, so "Jeremy's" (with a typographic apostrophe) arrives as
    "Jeremyâs".

## Not done

- No live UI clicks. The Chrome extension was not connected, so the pages were
  driven with the same form posts and API calls the page scripts make. Finding 8
  is what a browser would hit, derived from the shipped script and reproduced at
  the HTTP level rather than in a browser.
- The seed was skipped, so the create-once path for a device that has been
  `clean` since before the plugin existed was not exercised.
- No Discard: all seven proposals were applied.
