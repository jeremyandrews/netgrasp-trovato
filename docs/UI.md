# Netgrasp web UI: what each screen is for

The design for the netgrasp pages on kernel 0.104.0, written 2026-11-17 from the
live site (`docs/screenshots/before/`), the live data (read only) and the code.
Runs U1, U2 and U3 build it, in that order, with no kernel change. Anything that
needs a kernel change is listed under [Kernel follow-ups](#kernel-follow-ups)
and is not part of U1 to U3.

The goal, in Jeremy's words: an easy way to see what is online, to give devices
owners (create, merge and fix people along the way), and to be told when a
device comes online ("Jamie must be home"), with a menu that makes sense. On a
phone as often as on a laptop.

## What the live site says today

Counts read on 2026-11-17 (read only, `BEGIN READ ONLY`):

| | |
|---|---|
| Devices known | 166 |
| Online now | 76 (69 to 80 over the day) |
| With an owner | 0 |
| With a name a person gave (`display_name`) | 0 |
| Hidden | 0 |
| With a place (`current_location`) | 0. UniFi enrichment is off, so nothing has a place |
| `mdns_name` with a possessive ("Jeremy's Mac Studio") | 22 rows, 3 names: Jeremy (19), JAMIE (2), Me (1) |
| `notify = true` | 166 of 166. It is the column default |
| People | 0 |
| Alerts ever delivered (`ng_events.notified`) | 0. `[notify.ntfy]` is commented out in the daemon config |

Events, last 7 days: `ip_changed` 526,962; `arp_scan` 20,644; `name_updated`
2,469; `identity_change` 1,201; `returned` 561; `went_offline` 552;
`gratuitous_arp` 369; `ip_conflict` 21; `arp_spoof` 15; `new_device` 6; and
no `person_*` events, because there are no people.

What these numbers decide:

- **Nobody has sorted anything yet.** The to-do list is all 166 devices. Setting
  up means assigning dozens of devices at once, so bulk assignment is the most
  useful thing to build. Assigning one row at a time is too slow for this.
- **"Needs attention" has to be bounded.** On Home it shows the 6 new devices
  from this week, plus one line linking to the other 160 devices. It does not
  show 166 rows.
- **Owner suggestions from mDNS names are worth building, but only as
  suggestions.** 22 of 166 devices (13%) name their owner. Case varies (JAMIE),
  one name is wrong ("Me"), and one Mac shows up as several rows
  ("MacBook Pro (4)" three times). So the plugin suggests an owner and never
  assigns one without a person choosing it.
- **"By place" has nothing to show.** It stays available as a filter, with an
  empty state that says why, but it is not a menu entry.
- **The security total is noise.** 98% of it is `arp_scan` (20,644 in 7 days)
  and `identity_change`. Only `arp_spoof`, `rogue_dhcp` and `ip_conflict` are
  things a person can act on.
- **Every device would alert.** `notify` defaults to true, and the daemon
  alerts on `returned`. Once ntfy is configured, that is about 80 alerts a day
  (561 `returned` events in 7 days, minus debounce). The Alerts screen has to
  show this and let the reader turn it off.

What the screenshots show (`docs/screenshots/before/`, 18 pages at 1440 and
390 px, all 200):

- **The menu has 15 entries in one row.** At 1440 px it is wider than the window
  (the page renders 1593 px wide). At 390 px it wraps to six rows and pushes the
  content below the fold (`form-device-owner-390.png`).
- **Tables do not fit a phone.** At 390 px every device and event table scrolls
  sideways, the page renders 970 to 990 px wide, and the owner column and row
  menu are off screen (`overview-390.png`, `devices-online-390.png`).
- **The owner form is a dead end today.** Its only choice is "nobody", because
  no people exist and the form cannot create one (`form-device-owner-*.png`).
- **Anonymous visitors see everything.** `curl https://netgrasp.pozza/devices/online`
  with no session returns 100 MAC addresses. Gathers have no permission of
  their own, and `view netgrasp devices` only gates the menu entry. U1 deals
  with this first (see U1).

## Navigation

Five entries. Everything else is a tab on one of those five pages or is
reached from a row.

```
Home        /overview            who is home, today, needs attention
Devices     /devices             tabs: Online · All · New · To sort · Hidden · By place · By type
                                       + search box; /devices/assign (bulk)
People      /people              tabs: Everyone · Home now · Comings and goings
                                       + Add person; person page /item/<uuid>
Alerts      /alerts              rules, delivery status, recently sent
Activity    /activity            tabs: Notable · Comings and goings · New devices · Security · Everything
```

**Why tabs on the page, not a second menu level.** At 0.104.0 the kernel never
renders a child menu entry. `MenuRegistry::root_menus` keeps only entries with
no parent (`crates/kernel/src/menu/registry.rs:228`); `templates/page.html:20-40`
prints a flat row of `<a>` with no nested list, dropdown or toggle; a
`menu_link` row with a `parent_id` is printed flat beside its parent; and on a
phone the theme only wraps the row (`static/css/theme.css:1905`). So `MenuRoute::parent()` hides an entry and does nothing else. Tabs drawn by
the plugin's own gather templates work on every width, show which filter is
active (the kernel's active-link check is an exact path match, so the menu
cannot), and need nothing from the kernel.

**How the menu is replaced (U1, plugin migration).** The kernel draws one of two
menus. If the `menu_link` table has any row with `menu_name = 'main'` on the
live stage, it draws those rows and ignores every `tap_menu` entry
(`routes/helpers.rs:248`, `page.html:21`). The live table has 0 rows. A netgrasp
migration that inserts the five rows above (with `plugin = 'netgrasp'`, weights
0 to 4, and `ON CONFLICT (path, menu_name, stage_id) DO NOTHING`) removes Home,
Translate, Post, Blog, Terms, Translate Language and Media from the bar, sets
the order, and takes effect on the next request with no restart, because the
rows are read on every page. The `tap_menu` page entries stay in place as the fallback for a site that
empties `menu_link`.

Where the kernel entries come from: the kernel itself adds Home (`state.rs:527`).
The others are top-level `tap_menu` entries of plugins that are on by default:
`trovato_blog` (Blog, Post), `trovato_content_translation` (Translate,
Translate Language), `trovato_categories` (Terms) and `trovato_media` (Media).
None of them is seeded in a migration, and nothing in the install profile adds
them. **Disabling those plugins is a site decision, not netgrasp's.** It
belongs in the deployment's startup command (`./trovato plugin disable
trovato_blog`, and so on), which writes `plugin_status.status = 0` and takes
effect at the next start. A netgrasp migration should not do it: someone who
installs netgrasp on a site that has a blog would lose the blog. Keep
`trovato_redirects` enabled, because the 301s below depend on it. Two
limitations of `menu_link` to know about: it is not filtered by permission
(see [Kernel follow-ups](#kernel-follow-ups)), and the admin UI at
`/admin/structure/menus` can edit these rows, which is fine.

**Old paths.** Every current path keeps working. The one page that goes away,
`/who-is-home`, gets a real 301 through the `redirect` table, which a plugin
migration can write and `trovato_redirects` serves with a `Location` header.

| Path today | After | How |
|---|---|---|
| `/overview` | Home | unchanged |
| `/devices` (all devices) | Devices, Online tab | alias moves to `ng_device_online` |
| `/devices/online` | Online tab | unchanged alias, same gather |
| — | `/devices/all` | new alias for `ng_device_list` |
| `/devices/new`, `/devices/todo`, `/devices/type`, `/devices/owner`, `/devices/location` | tabs or facets of Devices | unchanged |
| `/who-is-home` | People, Home now tab | 301 to `/people/home` |
| `/people`, `/people/home`, `/people/movements` | People tabs | unchanged |
| `/events` | Activity, Everything tab | unchanged |
| `/events/security`, `/events/new-devices`, `/events/device` | Activity tabs or facet | unchanged |
| — | `/activity`, `/alerts`, `/people/add`, `/devices/assign` | new |

The existing `/gather/<id>` to friendly-path 301s (16 rows) stay, except that
`/gather/ng_device_list` is re-pointed from `/devices` to `/devices/all`.

## Screens

Each screen is described by its question, what is on it, what can be done from
it, where its data comes from, and what it shows when empty.

### Home: `/overview`

- **Question.** Who is here, what changed today, and does anything need me?
- **Content, in order.**
  1. **Home now.** One line per person who is home ("Jamie, home since 18:42,
     Kitchen"), and the people who are away in one muted line.
  2. **Today.** Arrivals and departures, newest first.
  3. **Needs attention.** At most 6 rows: new this week and not sorted, each
     with a one-tap **Assign** link to the owner form. Then a line such as
     "160 more devices have no owner. Sort them" linking to `/devices/assign`.
     If any `arp_spoof`, `rogue_dhcp` or `ip_conflict` happened in the last 24
     hours, one line per type, e.g. "2 ARP spoofing warnings today", linking to
     the Security tab.
  4. **Online.** A count that links to Devices ("76 devices online").
- **Removed.** The raw security total, and the "0 of 0 people home" card when
  there are no people. That card becomes the empty state below.
- **Data.** `ng_overview` (008), `ng_people_presence` and `ng_person_movements`
  (007), `ng_devices_new` (007/010). Security comes from a new view that counts
  the three actionable types over 24 hours.
- **Empty state, no people.** "Netgrasp doesn't know whose devices are whose
  yet. [Add the people in your home] or [assign devices in bulk]." That is
  exactly the live state today.

### Devices: `/devices`

- **Question.** What is on my network, and whose is it?
- **Tabs.** Online (default), All, New (this week), To sort, Hidden, By place,
  By type. To sort is `/devices/todo`: no name a person gave and no owner, as
  defined in 010, so naming the printer counts as sorting it. Each tab is its
  own gather route, the way this plugin already does facets (DESIGN.md
  Decision 6), so a tab never needs an exposed filter.
- **Search.** One box over name, MAC, vendor and IP. The device views gain a
  `search_text` column (lower-case `concat_ws` of `display_name`,
  `resolved_name`, `hostname`, `mdns_name`, `mac`, `vendor`, `last_ip`), and
  each tab's gather gets one exposed `contains` filter (`ILIKE`) on it. At
  0.104.0 a blank exposed filter is dropped, so it matches everything
  (`gather_service.rs:1180`). The box is a plain GET form in the plugin's own
  template, using `filter_values`, so it needs no token.
- **Row.** Name, owner, place (only when anything has a place), last seen,
  state, and the row menu: Rename, Assign owner, Hide, Alerts.
- **Actions.** The row menu, as today. **Sort several at once**
  (`/devices/assign`, below) from the To sort and New tabs.
- **Data.** The `ng_device_state` record and the 007/010 views. 0 rows hidden
  today, so the Hidden tab gets an empty state: "Nothing hidden. Hide a device
  to drop it from every list (routers, smart plugs)."
- **Empty states.** By place: "No device has a place. Places come from
  netgraspd's UniFi enrichment, which is off." Search: "No device matches 'x'."

### Sort devices in bulk: `/devices/assign` (a `tap_api` page)

- **Question.** These devices are Jamie's. Can I say so once?
- **Content.** A checkbox list of the to-sort devices (online first; 166 rows
  today, one `device` field per row), a person select plus "or a new person
  named [ ]", and one **Assign** button. Above the list, the suggestions from
  mDNS: "19 devices are named 'Jeremy's …': [select them]". That link reloads
  the page with `?suggest=jeremy`, which pre-ticks those rows. Nothing is
  assigned until the button is pressed.
- **Why it can be a page and not a gather.** `tap_api` gets a fresh
  `csrf_token`, reads the database through the `db` host, and parses repeated
  `device` keys from the raw `body`. `ApiRequest.query` keeps only the last
  value of a repeated key, so the selection must travel in a POST, never in the
  query string.
- **Write.** One `assist_host::apply_device_edit` per device, as the row form
  does. A new person is created first, through `apply_person_save`.

### People: `/people`

- **Question.** Who lives here, are they home, and which devices are theirs?
- **Tabs.** Everyone (home first, then away), Home now (`/people/home`),
  Comings and goings (`/people/movements`).
- **Row.** Name, home or away and since when, number of devices, alerts
  ("arrive" or "arrive, leave" or "off"), and the row menu: Rename, Alerts,
  Merge into…, Delete….
- **Action.** **Add person** (`/people/add`).
- **Data.** The `ng_person` Item gather, `ng_people_presence` and
  `ng_person_movements`.
- **Empty state.** "No people yet. A person is home when any device they own is
  online. [Add person]."

### Person page: `/item/<uuid>`

- **Content.** Name; home or away, since when, and where; their devices (the
  same rows as Devices); their last 20 comings and goings; their alert settings
  in words.
- **Actions.** Rename, Alerts, Merge into…, Delete…, and Add a device (the
  owner form, filtered to devices that have no owner).
- **How.** A `tap_item_view` for `ng_person`, like the device page
  (`device_view.rs`). Its links lead to the `tap_api` forms.

### Alerts: `/alerts` (a `tap_api` page)

- **Question.** What will I be told about, and will it reach me?
- **Content.**
  1. **Delivery.** "Alerts go to ntfy. Last alert delivered today at 18:42.",
     or "Alerts are not delivered anywhere yet." with the missing
     `[notify.ntfy]` named. This is read from `ng_notify_status`, one row the
     daemon (netgraspd schema V4) writes when its notifier starts and after
     every delivery attempt: `notify_configured`, `notify_backend`,
     `last_delivery_ok`, `last_delivery_at`. It names the transport and never
     the topic, server or token. A failed last attempt says so and points at
     netgraspd's log. Without a daemon that has V4, the row is absent and the
     page says "netgraspd has not said whether alerts can be delivered."
  2. **People.** One sentence per rule: "Tell me when Jamie gets home." and
     "Tell me when Jamie leaves." Each has an on/off switch-link (the existing
     person notify form).
  3. **Devices.** "166 devices alert when they come back online." Then
     **Only the devices I choose** (a `tap_api` bulk form, like
     `/devices/assign`, that sets `notify` false on every device not ticked),
     and the list of devices whose `notify` is on.
  4. **Recently sent.** The last 20 `ng_events` with `notified = true`, in
     words.
- **Add.** `/alerts/add` sets "Tell me when <person> gets home" or "leaves"
  through the person notify save, or "Tell me when <device> comes online"
  through the device notify save. Nothing new is written that those two forms
  do not already write.
- **Send test.** Shown when the daemon reports a destination. It inserts one
  row into `ng_notify_requests`; the daemon picks it up on its reconcile tick,
  sends one test alert ("Netgrasp test alert": "Alerts from netgraspd reach
  this device. Sent at 18:43."), and writes `handled_at` and an `outcome`
  word (`sent`, `failed` or `not_configured`). The confirmation page returns
  to `/alerts` after 12 seconds, where the outcome is shown. The plugin never
  sends anything.
- **Wording.** The daemon writes the alert text. An arrival reads "Jamie is
  home: Jamie's iPhone joined at 18:42" and a departure "Jamie left: last seen
  07:55", in the daemon's local time. It uses the name, not "her", because
  netgrasp stores no pronouns and should not guess them. Recently sent uses
  the same sentences.

### Activity: `/activity`

- **Question.** What happened, and is any of it worth my attention?
- **Tabs.** Notable (default: person movements, `new_device` and the three
  actionable security types), Comings and goings (`/people/movements`), New
  devices (`/events/new-devices`), Security (`/events/security`, actionable
  types first, with a link to the scan noise), Everything (`/events`).
- **Why Notable is the default.** `ip_changed` is 95% of all events (526,962 in
  7 days, about 52 a minute), so a log that shows everything shows nothing
  else.
- **Data.** A new gather over `ng_event` with `event_type IN (…)`. The
  existing event gathers serve the other tabs.

## People model

Every write goes through `assist_host` (`apply_person_save`,
`apply_device_edit`, `retire_person`, `item_host::delete_item`), the path the
assistant and the row forms already share. `ng_events` is daemon-owned and is
never rewritten.

| Action | Devices | Events | Alerts |
|---|---|---|---|
| **Create** (`/people/add`, or inline in the owner form) | none | none | `notify_arrive` and `notify_depart` off, as `create_person` does now. The form has an "Alert me when they get home" tick, off by default |
| **Rename** (existing form) | unchanged | history shows the new name: `ng_person_movements` prefers the mirror's current name | unchanged |
| **Merge** A into B (`/netgrasp/person/merge`) | every device of A gets owner B | A's past movements show B's name, through a plugin-owned `ng_person_merge (from_item_id, into_item_id, from_name, merged_at)` table that the movements view joins on | **B's settings win.** The form shows both and states which ones are lost: "Jamie (2) had arrival alerts on. Keep them for Jamie? [x]". The box is pre-ticked when either side had alerts on, so a merge never silently turns an alert off |
| **Delete** (`/netgrasp/person/delete`) | the form lists them and says "These N devices will have no owner". One POST unassigns them, then deletes | movements keep the name stored in `details`, as today | gone with the person |

Merge and delete are confirm pages, never one click. Merge refuses A = B.
Create refuses a name that already exists, case-insensitively (as
`create_person` does), and links to that person instead.

**Create inline.** The owner form gets "or a new person named [ ]". If the
field is filled in, one POST creates the person and assigns the device. If a
person with that name exists, the form assigns to them and says so ("Assigned
to the existing Jamie"). Since 0.4.0, netgraspd re-reads `owner_item_id` and
`ng_people` every `reconcile_interval` (10 s), so a new owner affects presence
within seconds. The restart limitation in `docs/JOINT-RUN.md` no longer holds.

## What the plugin can and cannot do at 0.104.0

| Need | Can? | How, or the workaround |
|---|---|---|
| Replace the top menu | yes | `menu_link` rows from a migration |
| Second menu level, dropdown, phone menu button | no | in-page tabs |
| Menu shows which section is active on sub-pages | no (exact path match) | the active tab is styled in-page |
| Search box | yes | exposed `contains` filter over `search_text`; blank matches all |
| Real 301 for old paths | yes | `redirect` table, with `trovato_redirects` on |
| 303 after a form POST | no (`ApiResponse` has no headers) | themed confirmation with meta refresh, as today |
| A form inside a gather row or a bulk select on a gather | no (no token in the gather context) | link to a `tap_api` page that renders its own form |
| Bulk assign | yes | `tap_api` page; repeated keys parsed from the POST body |
| Flash message after a write | no | the confirmation page names what changed |
| Person page | yes | `tap_item_view` for `ng_person` |
| Keep anonymous visitors out of device data | yes | `tap_field_access` denies every `ng_*` record field to a viewer without `view netgrasp devices`, and 025 takes `access content` from the anonymous role, so a visitor with no session gets the login page on every page and API |
| Show ntfy status, send a test, change alert wording | no kernel change | netgraspd V4: a status row the daemon writes, a request table the plugin inserts into, and the new wording |
| Cards on a phone instead of tables | yes | the plugin owns the table templates and `netgrasp.css` |

## Phone layout rules

1. **Nothing scrolls sideways at 390 px.** Below 640 px the device, event and
   people tables become stacked cards: name and the row-menu button on line 1;
   owner, state and last seen on line 2; MAC, vendor and IP on the device page.
2. **Tap targets are at least 44 by 44 px**, including the row-menu button and
   the tabs.
3. **Tabs stay on one line** and scroll sideways inside their own strip. The
   page itself never scrolls sideways.
4. **Home puts the answer first.** At 390 px the first screen shows "Home now"
   and "Needs attention" without scrolling.
5. **Forms are one column**, with the button full width and the label above the
   field.
6. **The menu takes at most two rows at 390 px.** That is five entries plus the
   theme's Admin, Profile and Logout links.
7. **Times are local and short** ("18:42", "Tue 18:42"), never ISO strings.

## The runs

Each acceptance check can be verified from a screenshot at 1440 px and 390 px,
taken logged in unless the check says anonymous.

### U1: navigation, Devices, Activity, Home cleanup, phone layout

Owns the `menu_link` migration (Home, Devices, People, Activity; U3 adds
Alerts), the `/devices` alias move, `/devices/all` and the `/who-is-home` 301,
device tabs and search, `/activity` and its tabs, the Home rework, the phone
card layout and tab strip, and `tap_field_access` for anonymous visitors.

Checks:

1. The menu reads exactly Home, Devices, People, Activity, then Admin, Profile
   and Logout. No Translate, Post, Blog, Terms or Media.
2. At 1440 px the page is not wider than the window. At 390 px the menu is at
   most two rows.
3. `/devices` shows the Online tab active. Each tab renders. Searching "motivo"
   shows only matching rows, and a blank search shows everything.
4. `/devices/online`, `/devices/todo`, `/events`, `/people/movements` and the
   other old paths render. `/who-is-home` answers 301 to `/people/home`.
5. Home shows no raw security total. "Needs attention" shows at most 6 rows and
   a link to the rest.
6. At 390 px, `/devices` and `/activity` render as cards, with no sideways
   scroll, and the row menu is visible on every card.
7. Anonymous `curl /devices/online` contains no MAC address. (Prove on the demo
   stack first that `tap_field_access` reaches record gathers: the kernel
   applies it to record pages at `gather_service.rs:1115`.)

### U2: People

Owns `/people/add`, "or a new person named" in the owner form,
`/devices/assign` with mDNS suggestions, the person page (`tap_item_view`),
merge (with `ng_person_merge` and the movements view change), delete, and the
People tabs.

Checks:

1. The owner form shows the person select and a "new person named" field.
   (Before/after screenshots are of the forms; the run proves the write on the
   demo stack, not on the live site.)
2. `/devices/assign` lists the to-sort devices with checkboxes and shows a
   suggestion line for each possessive mDNS name.
3. The person page shows devices, comings and goings, alerts, and the four
   actions.
4. The merge page names both people, the devices that move, and which alert
   settings win.
5. The delete page lists the devices that will lose their owner.
6. On the demo stack, after a merge, B owns A's devices and `/people/movements`
   shows B's name on A's old rows.

### U3: Alerts

Owns `/alerts` (delivery, people rules, devices, recently sent), the "only the
devices I choose" bulk form, and the Alerts menu row.

Checks:

1. The menu reads Home, Devices, People, Alerts, Activity.
2. On the live site, `/alerts` says "Alerts are not delivered anywhere yet."
   and names the missing `[notify.ntfy]`.
3. It says "166 devices alert when they come back online", with the bulk form
   linked.
4. With a person who has arrival alerts on (on the demo stack), the rule reads
   "Tell me when <name> gets home." with an off link.
5. Screenshots at 1440 and 390 px on the demo stack are in `docs/screenshots/u3/`:
   the Alerts page with rules and recent sends (`alerts-*.png`), the add form
   (`alerts-add-*.png`), and the Send test confirmation
   (`alerts-test-sent-*.png`, then `alerts-test-outcome-1440.png`). They were
   taken with netgraspd from the paired branch running against the demo
   database and posting to a local request logger, so "Jamie is home" and the
   test outcome there are real deliveries.

## Kernel follow-ups

These are not in U1 to U3.

1. **Render child menu entries**: a second level or dropdown, and a menu button
   below 768 px.
2. **Active trail by prefix**, so `/devices/new` marks "Devices" as active.
3. **Filter `menu_link` rows by permission.** Today every visitor sees every row.
4. **Gather access permission** (a `display.permission` or similar, answering
   403), so record gathers are not public by default.
5. **`ApiResponse` headers or `redirect()`**, for POST-redirect-GET
   (`G-API-RESPONSE-NO-HEADERS`).
6. **A form token in the gather template context**, for row forms and bulk
   selection on the listing itself (`G-GATHER-TEMPLATE-NO-CSRF`).
7. **A plugin flash-message API.** `messages` is in `base.html` but never filled.
8. **Keep repeated query keys.** `ApiRequest.query` keeps only the last value.

## Daemon and deployment follow-ups

These are outside this repository, and not in U1 to U3.

- **netgraspd: `notify` default.** Decide whether new devices should alert on
  `returned` by default. Today all 166 would.
- **netgraspd: `ip_changed` volume.** About 52 a minute. This is worth a look,
  and is probably IPv6 privacy addresses.
- **Deployment.** Add `./trovato plugin disable <name>` (one name per call)
  for `trovato_blog`, `trovato_media`, `trovato_categories` and
  `trovato_content_translation` to the startup command. Configure
  `[notify.ntfy]`.
