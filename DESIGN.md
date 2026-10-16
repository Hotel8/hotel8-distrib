# DESIGN.md: Hotel8:Distrib — Detailed Design Document

## 1. Scope & Context

**Owned by Distrib:** database `hotel8`, schema `distrib` — 12 tables
(`distrib.channels`, `channel_properties`, `room_type_mappings`,
`rate_plan_mappings`, `rate_updates`, `inventory_updates`, `bookings`,
`sync_log`, + `distrib.channel_type` / `distrib.sync_status` enums +
`distrib.outbox` / `distrib.audit_journal`); event family `distrib`; the
ARI-republication direct consumer; RFW admin pages.

**NOT owned:** room inventory/rates truth (hotel8:pms), actual reservation
state in-house (hotel8:pms holds it via received bookings), identity (Auth),
delivery of channel payloads (the channels are external systems — Bus events
describe business facts; transport contracts are per-channel integration).

### System context

```mermaid
graph TD
    subgraph "hotel8 vertical"
        D[hotel8:distrib]
        P2[hotel8:pms] -- "hotel8:pms:inventory:updated (consumer group distrib-ari)" --> D
        P2 -- "channel bookings pushed to PMS (HTTP)" --> P2
    end
    subgraph "platform"
        INGD[Sentin8:Proxy] -- "context headers" --> D
        AUTHD[Sentin8:Auth] -- "JWKS" --> D
        BUSD[Sentin8:Bus] <-- "produce (M2M)" --> RD[shell relay]
        RD --- D
    end
    CH[External channels] -. "OTA/GDS integrations consume ARI" .-> D
    DBD[(db hotel8 / schema distrib)] --> D
```

### Container view

```mermaid
graph TD
    subgraph distribd["hotel8:distrib"]
        CHA[/channels CRUD/]
        MAP[/mappings CRUD/]
        ARI[/ARI + content + controls push/]
        BK[/bookings intake/]
        SYN[sync engine + log]
        OUTD[(distrib.outbox)]
        AJD[(distrib.audit_journal)]
    end
    CHA --> SYN --> DBE[(distrib.* tables)]
    MAP --> SYN
    BK --> OUTD
```

## 2. Runtime Architecture

```mermaid
graph LR
    BOOT4[shell::serve] --> ENG4[Shield]
    BOOT4 --> RP4[(pool cap 3)]
    BOOT4 --> RL4[relay]
    BOOT4 --> JP4[audit pusher]
    CON4[consumer task: PMS inventory] --> REPUB[per-mapping ARI republish]
```

### Request lifecycle — ARI push

```mermaid
sequenceDiagram
    participant RMs as Revenue manager (Dash)
    participant D as hotel8:distrib
    participant S as Shield
    participant DB as db hotel8 (distrib)
    RMs->>D: POST /v1/channels/{id}/ari {rates, avail}
    D->>S: require(hotel8:distrib:ari:write)
    S-->>D: permit (edit-window ABAC check)
    D->>DB: BEGIN, INSERT distrib.rate_updates + inventory_updates + sync rows, outbox (hotel8:distrib:ari:pushed), journal, COMMIT
    D-->>RMs: 200 pushed-count
    Note over RMs,DB: channel deliveries reconcile via sync_log, sync:retried on failure
```

### Request lifecycle — channel booking intake

```mermaid
sequenceDiagram
    participant CH as Channel integration
    participant D as hotel8:distrib
    participant PMSx as hotel8:pms
    participant DB as db hotel8 (distrib)
    CH->>D: POST /v1/bookings {channel booking}
    D->>DB: BEGIN, INSERT distrib.bookings, outbox hotel8:distrib:reservation:received, COMMIT
    D->>PMSx: create reservation via PMS API (mapping-resolved ids)
    alt PMS accept
        D->>DB: sync state → synced, push status update event
    else PMS reject (sold out)
        D->>DB: sync state → failed/try, channel receives rejection path
    end
```

## 3. Data Model

```mermaid
erDiagram
    distrib_channels ||--o{ distrib_channel_properties : onboards
    distrib_channel_properties ||--o{ distrib_room_type_mappings : maps
    distrib_channel_properties ||--o{ distrib_rate_plan_mappings : maps
    distrib_channels ||--o{ distrib_sync_log : logs
    distrib_channel_properties ||--o{ distrib_inventory_updates : pushes
    distrib_channel_properties ||--o{ distrib_rate_updates : pushes
    distrib_channels ||--o{ distrib_bookings : receives
```

- `channel_type`: OTA/GDS/DIRECT/WHOLESALER/CORPORATE/METASEARCH (enum).
- `sync_status`: pending/synced/failed/retry (enum) — the SyncState machine
  mirrors it.
- All env-scoped + FORCE-RLS; audit journal included; channel secrets are
  NOT stored here (meta owns channel credentials).

### State machine — sync

```mermaid
stateDiagram-v2
    [*] --> PENDING: push enqueued
    PENDING --> SYNCED: channel accepted
    PENDING --> FAILED: terminal error
    PENDING --> RETRY: transient error
    RETRY --> PENDING: backoff elapsed
    FAILED --> [*]: manual replay path
    SYNCED --> [*]
```

## 4. API Surface

**Public** (`hotel8:distrib:{read,admin,ari:write}` scope mirror):

- Channels CRUD (`channel:created/updated/deleted`).
- Channel-property onboarding; room-type/rate-plan mappings.
- Rate markup/markdown configuration (`rate_markup:set`).
- ARI + content + controls push (`ari:pushed`, `content:pushed`,
  `controls:pushed`, `controls:set`).
- Reservation intake + status pushing (`reservation:received/updated/
  status_pushed`); sync replay (`sync:retried`).

**Admin/RFU**: RFW pages + shared policy admin (per-channel ARI edit
windows as ABAC; channel-partner edges as ReBAC).

### Cross-service data access — decision order (membus → bus → owner API)

ARI fan-out is event-first: `hotel8:pms:inventory:updated` (consumer group
`distrib-ari`) re-maps availability per channel — the bus path. Owner-API
residuals are synchronous request/response that must return a value:
channel booking intake pushes reservations into PMS (`create_reservation` —
the caller needs the confirmation number), ARI pull reads the 30-day
inventory window (`get_inventory`), cancellations propagate
(`cancel_reservation`). MemBus: none (channel sync is horizon-scoped, not
tick-scoped). No pms-schema grant; PMS gains no distrib tables either.

### Event flow

```mermaid
sequenceDiagram
    participant DB as distrib.outbox
    participant R as shell relay
    participant B as Sentin8:Bus
    DB->>R: NOTIFY + 5s poll
    R->>B: M2M produce
    alt ok
        B-->>R: 200 → relayed
    else down
        DB->>DB: attempts++ → DLQ at max
    end
```

## 5. Cross-Cutting Concerns

- Auth/tenant/RLS/audit/drain/TLS/pool follow the platform contract (same
  shell construction as the other hotel8 services).
- Channel-facing integrations authenticate as channel M2M identities with
  `hotel8:distrib:admin` scoped grants; booking intake is validated at the
  Shield layer before any mapping resolution.

## 6. Failure Modes & Recovery

```mermaid
flowchart TD
    A[Channel delivery failure] --> B[sync_log row + RETRY state]
    B --> C{retries exhausted?}
    C -->|yes| D[FAILED — replay available]
    C -->|no| E[backoff then retry; sync:retried event]
    F[PMS reject on intake] --> G[booking stays channel-pending; error surfaced to channel]
    H[Bus outage] --> I[outbox persists; relay retry]
```

## 7. Concurrency & Performance Model

```mermaid
graph LR
    REQ4[requests] --> MW4[middleware]
    MW4 --> H4[handlers]
    H4 --> CH4[("pool cap 3")]
    RELY4[relay] --> B4[("bus stream distrib")]
    CON5[consumer] -- SSE --> B4
```

- Hot path: ARI republish from PMS inventory events — batch per channel
  property from a single event; channel delivery is downstream of the
  outbox, never inline.

## 8. Invariants, Insecure-Defaults Rejected, Unsafe Rationale

- Every mapping resolves both sides before any push; unresolved mappings
  cannot produce ARI.
- A received booking always has its channel attribution and a sync row
  recording the delivery outcome.
- Rate updates are append-only facts; markups never mutate history.
- No `unsafe`; no platform-DB access; channel secrets are by-reference
  (hotel8:meta owns them).
- Insecure defaults rejected: no unsigned channel intake; no cross-channel
  read authority.

## 9. Testing Strategy

```mermaid
graph TD
    U4[Unit — SyncState transitions + markup math] --> I4[Integration — wiring + policy parity]
    I4 --> E4[E2E — channel admin surfaces via BFF]
```

## 10. Open Questions

- Two-way content sync.
- Rate-parity anomaly detection.

---

**Design attribution:** hotel8:distrib and its underlying concepts were designed by Hamid Jahanzad <Hamid@Jahanzad.com>.
