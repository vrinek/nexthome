# NextHome v2 plan

Decided 2026-10-23. A Rust port of the Rails app, open to paying strangers,
driven by the customer's own AI over MCP.

## Decisions

- **Product.** A household buys a *relocation*: one city, EUR/USD 10 one-time,
  data live for six months, then archived (read-only, never deleted).
- **Interfaces, in priority order.** MCP server; web map on desktop; same web
  map on mobile. No native apps. NextHome hosts no LLM.
- **Division of labour.** NextHome is a spatial database. The mover's AI holds
  the criteria (rent, rooms, dates), reads listing pages, coaches, and writes
  any brief for an agent. The human looks at the heatmap to decide where to
  search; the AI evaluates each candidate it finds.
- **Auth for AI.** API key (bearer token) first. OAuth 2.1 with dynamic client
  registration before go-live, behind the same tools. Intended MVP audience:
  people whose AI takes a bearer header (Claude Code, OpenClaw, config-file
  clients). claude.ai connectors and ChatGPT wait for OAuth.
- **Onboarding.** Marketing page has a "copy this prompt" button above the
  fold. The prompt sends the AI to `/ai`, an AI-readable integration page.
  The AI starts the relocation; the human pays (Stripe Checkout) and pastes
  the API key that arrives by email. Two human actions, accepted for launch.
- **Household.** An account has members, each with their own email and API
  key. No roles. The first member comes from `start_relocation`;
  `invite_member` emails a key to the next one. Locations record which member
  added them.
- **Web.** A viewer: map, heatmap, filters, tasklist, marker popup with one
  button per state. No create or edit forms.
- **Costs.** vrnk pays Mapbox and Google. One global monthly soft cap and a
  fair-use cap per relocation; tools refuse with a clear message past either.
- **Support agent.** Not in this version. A `report_issue` tool stores the
  message; it is the hook for the software factory later.
- **Hosting.** Rust binary as a systemd service in a VM on vrnk's hardware,
  SQLite file on the VM's disk with a nightly copy, Cloudflare tunnel in
  front. No containers, no database daemon.
- **Stack.** axum, sqlx + SQLite in WAL mode (one process is the only
  writer; the job loop runs inside it), official Rust MCP SDK (`rmcp`),
  `async-stripe`, askama templates, `geo` crate for polygon set operations
  (replaces the Python shapely scripts). Mapbox GL JS on the map page as today.
- **Dropped in the port.** The four listing importers (the mover's AI reads
  listing pages instead), shared maps (agents want text, the AI writes it),
  the create/edit forms, the Python scripts.
- **License.** AGPL-3.0.
- **Built by a Warp software factory from day one.** Every task below is
  factory-sized: one paragraph of intent, done when its tests pass. Tasks are
  ordered; each assumes the ones before it.

## Actors and events

Actors: the mover and their household, their AI, NextHome, Stripe, Mapbox and
Google, vrnk as operator.

1. Prompt copied from the marketing page into the mover's AI.
2. Integration page read by the AI.
3. Relocation started by the AI with email and city; Checkout link returned.
4. Payment completed; relocation activated; API key emailed.
5. AI connected with the key. Partner invited, second key emailed.
6. Place types chosen (school, gym, work, ...).
7. Area populated from Google, or places added one by one; locations in inbox.
8. Places triaged: accepted or rejected.
9. Isochrones computed for accepted places; heatmap available on the web.
10. Human looks at the heatmap and tells the AI where to search.
11. Listing found by the AI, added as an apartment in inbox with its URL.
12. Listing evaluated: travel times per mode to the nearest places per type.
13. Apartment state advanced: accepted, contacted, waitlisted, booked.
14. Apartment booked; the relocation has reached its goal.
15. Cap reached, at any point, and the tool says so.
16. Relocation archived after six months.

## Data model

Ported from `db/schema.rb`, scoped under an account.

```
accounts        id, created_at
members         id, account_id, email, api_key_hash, created_at
relocations     id, account_id, city_name, lat, lon, zoom, paid_at,
                expires_at (paid_at + 6 months), archived_at,
                stripe_checkout_session_id
place_types     id, relocation_id, name, icon, color, search_type, search_keyword
                (seeded per relocation from today's db/seeds.rb; unique
                name/icon/color within a relocation)
locations       id, relocation_id, place_type_id, member_id, name, lat, lon,
                state, note, source_url, external_identifier,
                google_maps_payload, created_at, updated_at
                (unique relocation_id + source_url, unique relocation_id +
                external_identifier)
isochrones      id, location_id, minutes, geojson
evaluations     id, location_id, computed_at, rows (JSON text: place_type,
                destination_location_id, walk, bike, transit, car minutes)
jobs            id, kind, payload, run_after, attempts, last_error, done_at
api_usage       id, relocation_id, provider ('mapbox'|'google'), month, calls
issues          id, member_id, body, created_at
```

`City` becomes fields on `relocation`: one relocation is one city.

## MCP tools

All take the bearer key. The relocation is implied by the key unless the
account has several, in which case tools take `relocation_id`.

| tool | does |
|---|---|
| `start_relocation` (unauthenticated) | email + city name → geocodes the city, creates account, first member and relocation, returns a Stripe Checkout URL |
| `invite_member` | email → new member, key sent by email |
| `get_relocation` | city, expiry, members, counts per state and place type, usage against caps |
| `list_place_types` / `add_place_type` | per-relocation reference data |
| `list_locations` | filters: state, place_type, bounding box |
| `add_location` | name + (address or lat/lon or Google Maps link) + place_type + source_url + note; geocodes; enqueues isochrones; returns id + web URL |
| `update_location` | state, note, name, coordinates (re-enqueues isochrones, invalidates evaluation) |
| `delete_location` | |
| `populate` | place_type + center → Google nearby search, creates inbox locations (background job) |
| `evaluate_location` | the travel-time table below, computed on first call and cached |
| `tasklist` | today's `locations#tasklist` as structured data |
| `report_issue` | free text, stored |

### Evaluation

For one location: per place type, the three nearest accepted places by
straight-line distance, and for each the travel time in minutes by walk,
bike, transit and car. Walking comes from point-in-polygon against the
destination's stored walking isochrones, as a five-minute band, with no API
call. Bike, transit and car come from one Google route-matrix request with
all destinations in it. Stored on the location; invalidated when the
location moves or when a place type gains an accepted location. Returned as
text:

```
Inselstraße 10, Mitte        walk  bike  transit  car
Work: Alex office              45    18       22   15
School: Grundschule Neumark    12     5       14    7
Gym: Urban Apes                 8     4        -    6
Martial arts: none within 30 min by any mode
```

The web heatmap is the human's picture of the same data and is not exposed
over MCP.

### Integration page

`/ai` is plain markdown describing the tools, the auth flow, the two human
steps, and how to invite a partner. It is the spec the customer's AI reads.

## Tasks, in order

Each task: a paragraph for the factory, and what "done" means. Tests are the
acceptance criterion throughout.

### 1. Repository skeleton
New crate `nexthome`, AGPL-3.0 LICENSE, axum server with `/healthz`, sqlx
with SQLite (WAL mode, foreign keys on) and a migrations directory, tests
running against a temporary database file each, CI running fmt, clippy, test
with no services. Done when CI is green on an empty app.

### 2. Schema and migrations
The tables above as sqlx migrations, with the constraints listed. Done when
migrations apply from empty and a test inserts one row per table.

### 3. Accounts, members and API keys
Create account with first member, generate key (random 32 bytes, store hash,
return plaintext once), axum extractor that resolves `Authorization: Bearer`
to a member and account and rejects unknown keys with 401. Done when tests
cover valid, missing and revoked keys, and two members on one account.

### 4. Relocation lifecycle
Create relocation, seed its place types from the seed list, compute
`expires_at`, a daily job that sets `archived_at` on expired relocations, and
a write guard that refuses mutations on archived relocations. Done when tests
cover the six-month boundary and the guard.

### 5. Mapbox and Google clients
Port `lib/mapbox.rb` and `lib/google_maps.rb`: geocode address near a city,
nearby search with pagination, place details by id, place id from a shared
Maps link, isochrone by minutes. Add Google route matrix (origins,
destinations, mode). Fakes for tests; no live calls in the suite. Every call
increments `api_usage`. Done when each function has a test against recorded
fixtures.

### 6. Usage caps
Per-relocation monthly cap and global monthly cap, both config values. A
`check_cap(relocation, provider)` that errors with a human-readable message.
Clients from task 5 call it before every request. Done when tests cover both
caps hit and not hit.

### 7. Background jobs
`jobs` table, a tokio loop that claims one job at a time with a single
`UPDATE ... WHERE id = (SELECT ... LIMIT 1) RETURNING *` (SQLite has one
writer, so this is atomic), retries with backoff up to 5 attempts, records
`last_error`. Job kinds: `isochrones(location_id)`, `populate(relocation_id,
place_type_id, lat, lon)`, `archive_expired`. Done when tests cover claim,
retry and completion.

### 8. Locations
Create, update, delete, list with filters, scoped to the relocation, stamped
with the member. Port `Location#geocode` (Mapbox, then Google fallback,
reject if > 1° from the city), Google payload v2 handling, state enum and
colours, uniqueness on `source_url` and `external_identifier`. Creating or
moving a location enqueues the isochrone job and deletes its evaluation.
Done when tests cover each create path (address, coordinates, Maps link) and
duplicate rejection.

### 9. Isochrones and heatmap
Port `Isochrone.generate_for_location` (5, 10, 15, 20, 30 minutes) and the
two heatmap algorithms from `app/models/isochrone.rb`, using `geo` for union,
intersection and difference instead of the shapely scripts. Heatmap takes the
two place types to combine as parameters instead of hard-coding School and
Gym, and returns empty layers when either is absent. Done when tests
reproduce the solo and intersection heatmap on fixture polygons.

### 10. Evaluation
As specified above: nearest three accepted places per type by haversine,
walking band from point-in-polygon on stored isochrones, one route-matrix
call for the other modes, stored in `evaluations`, invalidated on location
move or on a new accepted place in any type. Text renderer for the table.
Done when tests cover the walking band from fixture polygons, the matrix
call with a fake, cache hit, and both invalidations.

### 11. MCP server, API-key auth
`rmcp` streamable HTTP endpoint at `/mcp`. Tools from the table above except
`start_relocation`, `invite_member` and `report_issue`. Each tool is a thin
call into tasks 4–10 plus JSON in and out. Errors from caps and the archive
guard surface as tool errors with the message intact. Done when an MCP client
test lists tools and round-trips `add_location`, `evaluate_location`,
`update_location`, `tasklist`.

### 12. Stripe Checkout, `start_relocation`, `invite_member`
Unauthenticated `start_relocation` tool and a matching web form. Creates
account, member and relocation unpaid, returns a Checkout URL. Webhook sets
`paid_at` and `expires_at` and emails the API key. `invite_member` creates
a member and emails their key. Unpaid relocations are deleted after 7 days
by the archive job. Done when tests cover the webhook (signature checked),
both email sends (fake mailer), and the unpaid cleanup.

### 13. Web: sign-in and map viewer
Email + key sign-in (the key is the password). Port `cities#show` as
`/relocations/:id/map`: the Mapbox GL page from `city_map_controller.js`,
state and place-type filters, isochrone outlines, heatmap layers, marker
popup showing name, note, evaluation table, and one button per state.
Responsive down to phone width. Done when a browser test loads the map with
fixture locations and changes a state from the popup.

### 14. Web: tasklist
Port `locations#tasklist` as a read-only list with the same state buttons.
Done when a browser test filters by place type and changes a state.

### 15. `report_issue`, `/ai` and marketing page
Tool stores to `issues`. `/ai` serves the integration document; `/` is the
marketing page with the copy-this-prompt button. Done when a test asserts
the prompt text and the `/ai` page mention every tool name registered in the
server.

### 16. Deployment
VM image or shell script: the binary as a systemd unit, `cloudflared`
tunnel, nightly `sqlite3 .backup` to a second disk, env file for secrets
(Mapbox, Google, Stripe, SMTP). Done when a fresh VM comes up and
`/healthz` is reachable through the tunnel.

### 17. Data migration from the Rails app
One-off script: read the Rails `db/production.sqlite3` (same engine), create
vrnk's account with two members and one relocation per city, copy place
types, locations and isochrones. Done when counts match and the map renders the Berlin data.

### 18. OAuth 2.1
Authorization server endpoints with dynamic client registration and PKCE,
as the Claude and ChatGPT connector docs require. Tokens map to a member the
same way an API key does. `/.well-known/oauth-authorization-server`. Done
when an MCP client completes the flow in a test and calls `list_locations`.

### 19. Go-live checklist
Caps set, Stripe live keys, error reporting, `/ai` reviewed by pasting the
marketing prompt into a fresh Claude and a fresh ChatGPT and watching them
reach `add_location` and `evaluate_location`.

## Out of scope for v2

Shared maps, listing importers, text fields beyond the note, criteria stored
in NextHome, public transport or bike isochrones for the heatmap, metro
lines, plus codes, Trello, breezometer, multiple cities per relocation,
roles, the support agent, any native app.
