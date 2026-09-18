# Changelog

Netgrasp for Trovato. Entries are grouped by the change, not by the file, and
each one says what was wrong underneath rather than what was edited: a line that
reads "fixed the rename tool" tells the next person nothing about why the next
tool will do it too.

## Unreleased

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
