# lastmile — implementation plan

A self-hosted list of the packages my household is expecting: who is
delivering each one, where it is, and the code to give the courier or the
locker. Packages come from emails (uploaded as `.eml` files) and text
messages shared to the app. Rules, built in the app (with an LLM's help) and
shared as plain text or files, turn them into packages.

This is a **proof of concept** for my homelab. Querying couriers directly
(§6) is left for after it has been used for a while.

Same stack and look as [dominium](https://github.com/PedDavid/dominium) and
[clavium](https://github.com/PedDavid/clavium). Start M0 by copying their
skeleton (CI, Dockerfile, Helm chart, Basecoat/Tailwind/htmx assets, themes,
⌘K palette, OIDC from clavium) rather than writing it again.

## 1. Decisions

| Topic | Decision |
|---|---|
| Scope | Incoming packages for one household: status, courier, tracking number, delivery or pickup code, pickup point, ETA. No outgoing shipments, no letters |
| Couriers | No courier is special in code. CTT / CTT Expresso and Paack are the first ones I'll write rules for, in the app |
| Platform | Kubernetes, single replica, same stack as dominium/clavium (Rust, axum, askama, Tailwind v4, Basecoat, htmx) |
| State | **SQLite on a PVC.** Unlike the other two apps, packages are data the app creates, so it can't live in git or ConfigMaps. One file, embedded migrations, backed up with the volume |
| Input | Text shared from the phone (Android: PWA share target; iOS: Apple Shortcut), a **dropbox** for exported emails (`.eml` upload), a paste box, manual entry. No mail forwarding and no SMS forwarding |
| Parsing | **Deterministic rules** run on every message. An LLM is never on the recurring path: it is only used to **write a new rule** from a message no rule matched, after PII redaction, and the rule is reviewed and saved by a person |
| Rules | **None ship with the app.** They are created in the app (by hand or drafted by the LLM) and shared as a serialized rule: copy/paste a text blob, or export/import a file. A shared catalogue ("marketplace") could come later on top of the same format |
| Tracking | The status comes from messages (e.g. "out for delivery") and manual changes, which always win. Courier adapters are designed for (§6) but not in the PoC |
| Auth | OIDC as in clavium. Every household member who logs in can view and add packages; admins (group claim) can also edit rules and use the LLM. The iOS Shortcut uses per-user ingest tokens |
| Alerting | Prometheus metrics plus an optional `PrometheusRule` (e.g. waiting at a pickup point for too long). No notifier in the app |

## 2. What already exists, and why it isn't used

Checked in October 2026; re-check anything marked *unverified* during the spikes.

| Option | Why not (as the primary path) |
|---|---|
| Home Assistant core `17track` / `aftership` | Depend on paid or quota-limited aggregator accounts; AfterShip no longer has a useful free tier; HA's integrations only expose counts and states, not codes |
| [parcel-ha](https://github.com/jmdevita/parcel-ha) (Parcel app) | Needs a Parcel premium subscription (Apple only), strict rate limits (about 20 req/h), and depends on Parcel supporting the courier |
| `parcelsapp.com` integration | Free tier is about 10 parcels a month |
| Mail and Packages (HACS), Paket Tracker, PostNL, Informed Delivery | Email parsers for US, German and Dutch carriers. None handle CTT or Paack, and the codes aren't extracted |
| [jupitern/ctt](https://packagist.org/packages/jupitern/ctt) (PHP) | Not usable as is, but it proves CTT's public object search page can be scraped. Use it as a reference for the CTT adapter |
| Aggregator APIs: 17TRACK, Ship24, TrackingMore | 17TRACK is sold as yearly quota packs (unverified); Ship24's free tier is 10 shipments a month; TrackingMore is paid. Paack coverage is unverified everywhere. Could become an optional adapter (§6) |

The common gap: none of them extract **the code to give the courier**, and
the codes are the main reason for this app. They only arrive in the courier's
messages, so message parsing is the core of the app and the courier APIs are
an extra, for later.

## 3. Architecture

```
 email client ── export .eml ──▶ dropbox (upload, POST /inbox/upload) ──┐
 Android share ──▶ PWA share target (GET /share) ────────────────────────┤
 iOS share sheet ──▶ Shortcut ── POST /api/ingest (token) ───────────────┤
 UI paste box / manual form ─────────────────────────────────────────────┤
                                                                         ▼
                    ┌──────────────────────────────────────────────────┐
                    │ lastmile (single binary, single replica)         │
                    │  messages ──▶ rule engine ──▶ package upsert     │
                    │                   │ no match                     │
                    │                   ▼                              │      ┌──────────────────┐
                    │                inbox ──▶ redact ──▶ LLM ─────────┼────▶ │ Claude API       │
                    │                            draft rule ──▶ review │      │ (admins, opt-in) │
                    │  rules ◀──▶ export / import (clipboard, file)    │      └──────────────────┘
                    │  SQLite (PVC) · axum UI · /metrics               │
                    └──────────────────────────────────────────────────┘
                                       │ /metrics
                                       ▼
                             Prometheus ──▶ Alertmanager
```

- **One replica**, SQLite in WAL mode on a `ReadWriteOnce` PVC, Deployment
  strategy `Recreate`. No leader election.
- **Background tasks** in the same process: the retention sweep (and the
  courier poller, once there is one).
- **Metrics** are computed from the database on scrape (the counts are small).

## 4. Data model

```
packages      id, title, merchant, recipient (household member, optional),
              courier, tracking_number, order_ref, tracking_url,
              status, status_source, status_at, eta_from, eta_to,
              code, code_kind (delivery | pickup), code_expires_at,
              pickup_point (name, address, hours), notes,
              created_at, archived_at
events        id, package_id, at, status, description, source (courier | message | manual), raw_ref
messages      id, received_at, channel (upload | share | api | paste), submitted_by,
              sender, subject, body_text, raw (MIME or text), rule_id, rule_version,
              result (matched | unmatched | ignored | error), package_id, purge_at
rules         id (UUID, stable across export/import), version, name, enabled, definition (JSON),
              origin (local | imported), created_by, created_at
rule_samples  rule_id, message_id or stored redacted text, expected (JSON)
tokens        id, user, name, hash (argon2 or SHA-256 of 32 random bytes), last_used_at, revoked_at
```

**Status.** Six internal states, shown as the three filters asked for, plus a
flag:

| Filter | Internal states |
|---|---|
| Preparing | `announced` (order confirmed, label created) |
| In transit | `in_transit`, `out_for_delivery` |
| Arrived | `ready_for_pickup` (at a locker or shop, code needed), `delivered` |
| *Needs attention* (flag, not a state) | `exception` (failed attempt, returned, customs), or no update for `staleAfter` (default 7d), or pickup deadline near |

Status only moves forward, except `exception`, and manual changes win
whatever the order. An event with an older timestamp than the current status
is recorded in the timeline but doesn't change the status.

**Merging.** A message is linked to an existing package by
`(courier, tracking_number)`, then by `(merchant, order_ref)`. If neither
matches but the rule says the message is a follow-up (e.g. "ready for
pickup" with no number), it goes to the inbox with the candidates listed for
a one-click attach. The app never guesses.

**Retention.** `delivered` packages are archived after 7 days (hidden from the
default list, kept for search). Raw messages hold personal data, so `raw` and
`body_text` are purged after 30 days (`--message-retention`) unless the
message is pinned as a rule sample.

## 5. Ingestion

All channels produce a `message` and run the same pipeline. Every channel
accepts plain text; uploaded emails also keep the MIME.

**Email dropbox.** On the inbox page, a drop zone and file picker that take
one or more `.eml` files (Gmail: *Download message*; Apple Mail and
Thunderbird: drag the message out or *Save as*). Up to 10 files and 5 MiB
each per upload, as a normal CSRF-protected multipart POST. The app parses
the MIME with `mail-parser` (sender, subject, date, text part, or the HTML
part converted to text) and runs the rules. The page then lists each file
with its result. A message already uploaded (same `Message-ID`, or same
hash of the raw file) is reported as a duplicate instead of being processed
again. Outlook `.msg` files are not supported. No mailbox access, no inbound
SMTP.

**Android: PWA share target.** The app ships a manifest with
`share_target` (method **GET**, `params: {title, text, url}`) and a minimal
service worker so it can be installed. Sharing opens `/share?text=…` (GET
keeps the `SameSite=Lax` session cookie, which a cross-site POST would drop)
with the text in a confirm form. The person checks it and submits, through
the normal CSRF-protected POST. `/share` must be excluded from request
logging, or at least have its query string stripped.

**iOS: Shortcut.** The share sheet in Messages only hands over text, so
there is no sender. A documented Shortcut takes the share sheet input and
POSTs it to `/api/ingest` with `Authorization: Bearer <token>` (JSON
`{text, channel: "share"}`), then shows the reply as a notification
(*"CTT RR123456789PT – ready for pickup, code 482913"* or *"No rule
matched – in the inbox"*). Tokens are created and revoked per user under
*Settings → Devices* and are stored as hashes. Only the ingest endpoint
accepts tokens.

**Paste and manual.** A paste box on the inbox page (same pipeline). A manual
form takes courier, tracking number, title and code; the courier is
auto-detected from the tracking number format where possible.

Since shared texts have no sender, **rules must be able to match on content
alone**.

## 6. Couriers (after the PoC)

Not built in the PoC: packages show the tracking link the rule extracted,
and the status moves with messages and manual changes. This section is the
intended design, so the data model leaves room for it (`events.source =
courier`, `status_source`). Checking whether CTT and Paack can be queried
reliably comes first.

```rust
#[async_trait]
trait Courier: Send + Sync {
    fn id(&self) -> &'static str;                         // "ctt", "paack"
    fn recognises(&self, tracking: &str) -> bool;         // number format, for auto-detect
    fn tracking_url(&self, tracking: &str) -> Option<Url>;
    async fn track(&self, tracking: &str) -> Result<Vec<CourierEvent>, TrackError>; // Unsupported if no adapter
}
```

- **CTT / CTT Expresso.** No public developer API for recipients is known
  (CTT has business APIs under contract, unverified). Adapter: the public
  object search page on ctt.pt, as `jupitern/ctt` does. Map CTT's event
  descriptions to the six states in a table with tests. Tracking numbers in
  the UPU S10 format (`[A-Z]{2}\d{9}[A-Z]{2}`), plus CTT Expresso's own
  formats (collect from samples).
- **Paack.** No public API. Its messages carry a tracking link. **Spike:**
  check whether the tracking page loads a JSON endpoint that can be called
  with the token from the link. If not, Paack is message-only: the status
  comes from the SMS/emails, and the detail page links to the tracking page.
- **Polling.** Active packages with an adapter are polled every 2h (spread
  out, one request at a time per courier, with a fixed user agent naming the
  project URL). Back off on errors. Stop polling
  when delivered, or after 30 days without changes (then flag as stale).
- **Hosts are fixed in code**, as with clavium's probes.
- **Later, optional:** an `aggregator` adapter (17TRACK or Ship24) behind the
  same trait, with a monthly quota in the settings, for couriers without a
  direct adapter.

## 7. Rules (the "workflows")

A rule is declarative data, not code: match a message, extract fields with
regexes, set a status. Rules are stored in SQLite and versioned (a saved edit
is a new version; messages record which version matched them). The app ships
with **no rules**: the database starts empty, and rules are written in the
app, drafted with the LLM (§7.1) or imported (§7.3). `--demo` loads a few
example rules with synthetic messages.

```yaml
id: 3f6c…                    # UUID, kept on export/import
slug: ctt-ready-for-pickup
name: CTT – ready for pickup (SMS)
courier: ctt
match:                       # all present conditions must hold
  channels: [share, email, api, paste]
  sender: ["*@ctt.pt", "CTT"] # optional; shared texts have none
  all: ["(?i)disponível para levantamento"]
  none: ["(?i)devolvid"]
extract:                     # Rust `regex` syntax, named group `v` is the value
  tracking_number: '\b(?P<v>[A-Z]{2}\d{9}PT)\b'
  code: '(?i)código[^0-9]{0,20}(?P<v>\d{6})'
  pickup_point: '(?i)em (?P<v>[^.\n]+?)(?:\.|\n|$)'
  eta: null
set:
  status: ready_for_pickup
  code_kind: pickup
priority: 100                # highest wins; ties go to the most specific match
samples: [msg_01J…]          # regression samples, re-run on every save
```

- **Engine.** Every enabled rule is tried, and the highest-priority match
  wins. A required field (tracking number or order ref, unless the rule says
  `followUp: true`) that fails to extract makes it an `error` result shown in
  the inbox, not a silent miss. Text is normalised first (NFC, whitespace,
  quoted-printable and HTML entities already decoded).
- **Regex limits.** The `regex` crate has no look-around or backreferences;
  the editor and the LLM prompt must say so. Size limits on compiled regexes.
- **Editor (admin).** Shows the rule as YAML with a live preview: the
  extracted fields for each sample, and every inbox message it would now
  match. Saving runs all samples of all rules and refuses a rule that changes
  another rule's expected result unless confirmed.
- **Tests.** The engine is tested with **synthetic** messages written in the
  style of CTT and Paack (`tests/fixtures/messages/*.{eml,txt}`, each with an
  `*.expected.json` and the rule it is run with), so no real personal data is
  committed. The same fixtures feed `--demo`.

### 7.1 Building a rule with the LLM

Only from the inbox, only for admins, only when an API key is configured.

1. **Redact.** The message goes through the redaction step (§7.2). The admin
   sees the redacted text side by side with the original and can redact more
   by selecting text. Nothing is sent until they click *Send*.
2. **Draft.** One call to the Claude API (model configurable; use structured
   output / a tool with the rule's JSON Schema so the reply is always a valid
   rule). The prompt includes the rule schema, the regex dialect limits, the
   six states, the redacted message, the existing rules for the same courier
   (so it follows their style and priority), and what the admin says it is
   (*"Paack out for delivery, the 4 digits are the code"*). The model returns
   a rule and a short explanation; it is asked to extract nothing from the
   placeholders.
3. **Validate locally.** Compile the rule, run it on the *original*
   unredacted message and on all samples. Show the extracted fields. A
   draft that doesn't compile, doesn't match the message or breaks another
   rule's samples can be sent back with the error (at most 3 automatic
   retries).
4. **Review and save.** The admin edits if needed and saves; the message
   becomes the rule's first sample (stored redacted, unless they choose to
   keep the original) and is reprocessed, creating the package. The other
   messages in the inbox are re-run too, since the new rule may match them.

The API key is a Secret mounted as a file (tracked in clavium, synced from
OpenBao by ESO, as with any other key). Calls, tokens used and failures are
counted in metrics.

### 7.2 Redaction

A separate module (`redact.rs`) with its own tests, used for anything sent
outside the cluster. It replaces personal data with typed, numbered
placeholders that keep the text's shape, e.g. `‹NAME_1›`, `‹ADDRESS_1›`,
`‹PHONE_1›`, so the model still sees where things are.

- **Known values (strongest).** Household names, addresses, postcodes,
  phone numbers and email addresses from the settings, matched ignoring
  case and accents.
- **Patterns.** Email addresses, phone numbers (PT `+351` / 9 digits and
  international), PT postcodes (`\d{4}-\d{3}`), NIF (9 digits, with the
  check digit validated to avoid false positives), IBAN, card numbers (Luhn),
  query strings and path tokens of URLs (keep the host), and the greeting
  line's name (`Olá <name>`, `Caro(a) <name>`).
- **Optional analyzer.** A Presidio analyzer in the cluster (`--presidio-url`)
  is asked for people and places in the original, and what it finds gets the
  same placeholders, below the household values and the patterns in priority.
  It is only a suggestion on the draft page: if it is down the page says so,
  and the household values still apply.
- **Not redacted:** tracking numbers, delivery/pickup codes, merchant and
  courier names, dates. They are what the rule has to find. The code is
  replaced with a same-length placeholder of the same character class
  (`482913` → `‹CODE:dddddd›`) only if the admin ticks it.
- **Tests:** a no-leak test like clavium's: every configured household value
  in a fixture must be absent from the outgoing request body (wiremock).
- **Also used for exports** (§7.3): samples leave the app only in redacted
  form.

### 7.3 Sharing rules

A rule is shared as a self-contained, versioned document. It is the first
step towards a shared catalogue, so the format has to stand on its own
without the app's database.

```yaml
lastmile: rule/v1             # format marker and version
id: 3f6c…                     # same UUID across copies, so re-importing updates instead of duplicating
version: 4
slug: ctt-ready-for-pickup
name: CTT – ready for pickup (SMS)
description: CTT's SMS when a parcel reaches a locker or shop.
courier: ctt
locale: pt-PT
match: { … }
extract: { … }
set: { … }
samples:                      # redacted only, with the expected fields
  - text: "CTT: A sua encomenda RR123456789PT está disponível para levantamento em ‹ADDRESS_1›. Código: 482913."
    expected: { tracking_number: RR123456789PT, code: "482913", status: ready_for_pickup }
```

- **Export** one rule or a selection, as either a **copy-to-clipboard blob**
  (the YAML, or the same YAML gzipped and base64url-encoded in a single line
  starting `lastmile:rule:v1:`, for chat apps that mangle multi-line text) or
  a **`.lastmile.yaml` file** download. Samples go through redaction first
  and the admin sees the export before copying it.
- **Import** from a paste box or a file. The app parses and validates it
  (schema, regexes compile within size limits, samples produce their
  expected fields), then shows a preview: the fields, the samples' results,
  which inbox messages it would match, and whether it is new or updates an
  existing rule with the same `id` (with a diff if so). Imported rules are
  saved **disabled**, with `origin = imported`, until the admin enables them.
- **Imported rules are untrusted input.** They are data, never code: only
  the fixed rule schema, Rust `regex` (linear time, no catastrophic
  backtracking) with compiled-size limits, and a cap on the number and size
  of patterns. That is what makes sharing, and later a catalogue, safe.
- **Later:** a catalogue (a git repo of `.lastmile.yaml` files, or a small
  index the app can browse) would reuse this format as is, plus a signature
  or checksum.

## 8. UI

Mobile first: it will mostly be opened from a phone at the door. Same
components, themes and ⌘K palette as the other apps.

- **Packages (`/`).** Filter chips *Preparing · In transit · Arrived* plus
  *Needs attention* and *Archived*, text search, recipient filter. Cards
  on narrow screens and a table on wide ones: title/merchant, courier badge,
  status with relative time, ETA, and the **code in large monospace with a
  copy button** (and pickup point when it is `ready_for_pickup`). Sorted:
  ready for pickup and out for delivery first, then by ETA.
- **Package detail (`/packages/{id}`).** Code, pickup point (map link),
  tracking link, timeline of events with their source, linked messages,
  edit/override status, archive.
- **Inbox (`/inbox`).** Unmatched and errored messages, the paste box and
  the `.eml` dropbox, and per message: *Attach to package*, *Create
  package*, *Ignore*, *Build rule* (admins).
- **Rules (`/rules`, admin).** List with hit counts and last match, editor
  with live preview, *Copy* / *Export file* / *Import*. An empty state
  points to the inbox and to importing.
- **Settings.** Ingest tokens per user, Shortcut setup instructions, the
  household values used for redaction (admin).

## 9. Metrics and alerts

Served on `:9090/metrics` with `/healthz` and `/readyz`, as in the other apps.

| Metric | |
|---|---|
| `lastmile_packages{status, courier}` | Active (non-archived) packages |
| `lastmile_package_ready_since_timestamp_seconds{id, courier}` | For packages waiting at a pickup point |
| `lastmile_package_code_expiry_timestamp_seconds{id}` | When the courier says the pickup ends, if known |
| `lastmile_inbox_messages{result}` | Unmatched / errored messages waiting |
| `lastmile_messages_total{channel, result}` | |
| `lastmile_rules{origin, enabled}` | |
| `lastmile_llm_requests_total{result}`, `lastmile_llm_tokens_total{kind}` | |
| `lastmile_analyzer_requests_total{result}` | |

Shipped alert rules: `PackageWaitingForPickup` (ready for more than 3 days,
CTT holds items for a limited time), `PackagePickupEnding`,
`PackageInboxNotEmpty` (info, for 1d), `LastmileDown`.

## 10. Security

- Same as clavium: OIDC with PKCE, encrypted session cookies, same-origin
  `Origin` check plus CSRF token on POSTs, strict CSP (`script-src 'self'`),
  `Cache-Control: no-store`.
- `/api/ingest` takes bearer tokens only (no cookies), limited body size
  (256 KiB), rate limited per token. Tokens are hashed and shown once.
- Uploaded `.eml` files: size and count limits, parsed in memory, never
  rendered as HTML (text only), attachments ignored.
- Imported rules are validated data with bounded regexes (§7.3).
- Message bodies are personal data: never logged above DEBUG, purged on a
  schedule, redacted before anything leaves the cluster. The LLM is off
  unless an API key is configured.
- The LLM host (and, later, courier hosts) are fixed in code.
- The ingest endpoint and the UI must be reachable from the phones when
  away from home: put the Ingress behind the usual setup (or a VPN). Only
  `/api/ingest` needs to work without an OIDC session.

## 11. Project layout

```
src/
  main.rs  config.rs  demo.rs  metrics.rs  duration.rs
  db/{mod,migrations}.rs   # rusqlite (bundled), WAL, embedded SQL migrations, spawn_blocking
  model.rs                 # Package, Event, Message, Status
  ingest/{mod,eml,share}.rs  # .eml upload → text, share target, ingest API
  rules/{mod,engine,schema,share}.rs  # share = rule/v1 export/import
  redact.rs
  llm.rs                   # Claude API client, prompt, validation loop
  sweep.rs                 # retention, archiving
  web/{mod,auth,pages,api,views}.rs
templates/  assets/        # as in dominium/clavium, plus manifest.webmanifest and sw.js
tests/fixtures/messages/   # synthetic only
tests/fixtures/rules/      # example rule/v1 files, also loaded by --demo
deploy/helm/lastmile/      # Deployment (Recreate), PVC, Secrets refs, ServiceMonitor, PrometheusRule
```

Crates on top of the shared stack: `rusqlite` (bundled), `mail-parser`,
`html2text`, `regex`, `flate2` and `base64` (clipboard blobs), `uuid`,
`sha2` for tokens, `unicode-normalization`. Tests: `insta`, `wiremock`, and
the Playwright harness from clavium.

## 12. Milestones

**M0 – Skeleton.** Copied stack, CI, Dockerfile, Helm chart with PVC,
SQLite and migrations, `--demo` with sample packages in a temp database.

**M1 – Useful by hand.** OIDC, packages list with filters, detail, manual
add/edit, status override, archive, metrics. *After M1 the household can
already keep its list, codes included.*

**M2 – Messages and rules.** Paste box, `.eml` dropbox, ingest API and
tokens, PWA share target, iOS Shortcut docs, rule engine, rule editor (by
hand) with live preview and samples, inbox with attach/create/ignore,
retention, synthetic fixtures. *After M2 a rule written once handles every
later message of that kind.*

**M3 – Rules with the LLM, and sharing.** Redaction module and no-leak test,
LLM drafting with local validation, `rule/v1` export/import (clipboard and
file) with the import preview.

**M4 – Polish.** ⌘K palette, screenshots and visual tests, Grafana
dashboard, alerts.

**After the PoC.** Courier adapters (§6) once CTT and Paack have been
checked, an optional aggregator adapter, a shared rule catalogue (§7.3).

## 13. Not doing (for now)

- Forwarding email to the app, reading a mailbox (IMAP, Gmail API) or
  receiving SMTP. Emails come in as uploaded `.eml` files.
- Forwarding SMS automatically (needs an SMS gateway or an app on the phone).
- Rules shipped with the app.
- Querying couriers (§6).
- Push notifications from the app: alerts go through Alertmanager.
- The LLM extracting packages directly. It only writes rules.
- More than one household, outgoing shipments, returns.
