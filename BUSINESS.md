# Hotel8:Distrib - Business Requirements & Functionality

> Audience: product owners, business analysts, and anyone evaluating what
> Hotel8:Distrib **does for the business**. For architecture read
> `README.md`; for wire-level detail read `DESIGN.md`.

## 1. Business Purpose

- Sell hotel inventory across OTA/GDS/wholesaler/corporate channels with
  per-channel commercial terms.
- Guarantee the property's live availability reaches channels fast, and that
  channel bookings land in PMS as full-fidelity reservations.

## 2. Business Scope (What the Service Provides)

| Capability | Description |
|---|---|
| Channel onboarding | Channel registry with type (OTA/GDS/direct/wholesaler/corporate/metasearch) |
| Property mapping | Channel-side property ↔ hotel property translation |
| Inventory mapping | Room-type and rate-plan mappings per channel property |
| ARI distribution | Availability/rate/controls pushes with per-channel markups and edit windows |
| Content push | Channel content publication |
| Booking intake | Channel reservations with status pushback |
| Sync ledger | Per-update delivery tracking with retries and replay |
| Admin console | RFW pages incl. shared policy admin |

**What the service does NOT own (business boundary)**

| Concern | Owning service |
|---|---|
| Physical inventory & rate truth | hotel8:pms |
| In-house reservation lifecycle | hotel8:pms |
| Metasearch ad bidding | hotel8:meta |
| Channel credential storage | hotel8:meta |
| Money settlement of channel bookings | Bill8:Payments / Bill8:Ledger |

## 3. Business Capabilities

| Area | Business rule |
|---|---|
| Onboarding | A channel sells only mapped properties; unmapped inventory is invisible to it. |
| Markups | Channel rates derive from PMS retail rates plus the configured markup/markdown - deterministic and auditable. |
| Edit windows | ARI changes respect each channel's commercial edit window (policy-authored), protecting commitments near arrival. |
| Bookings | A channel booking maps to a PMS reservation; rejection (e.g. sold out) is surfaced to the channel and recorded. |
| Sync | Every ARI/content/booking push is tracked pending→synced/failed/retry; failures replay. |
| Content | Channel content sets are separate from rates so marketing text changes don't touch rates. |
| Controls | Stop-sell/min-LOS/max-LOS controls push per channel property. |

## 4. Business Events (Outbound)

| Event | Fires when |
|---|---|
| `hotel8:distrib:channel:created / updated / deleted` | Channel registry changes |
| `hotel8:distrib:channel_property:mapped`, `room_type:mapped`, `rate_plan:mapped` | Mapping changes |
| `hotel8:distrib:rate_markup:set`, `controls:set` | Commercial configuration |
| `hotel8:distrib:ari:pushed`, `content:pushed`, `controls:pushed` | Push execution |
| `hotel8:distrib:reservation:received / updated / status_pushed` | Channel bookings |
| `hotel8:distrib:sync:retried` | Delivery replay |

**Inbound (consumed):** `hotel8:pms:inventory:updated` - republishes ARI per
mapping.

## 5. Business Rules & Invariants

1. A channel never sees unmapped inventory.
2. Channel rates always derive from PMS retail rates + recorded markup -
   never hand-set deltas.
3. ARI is deterministic for a given inventory state + markup - replays are
   safe.
4. Sync attempts are capped; exhausted rows remain for manual replay rather
   than vanishing.
5. Every received booking retains its channel attribution permanently.
6. Channel edits respect their commercial windows (tenant-authored policy).

## 6. Target-User Roles & Use Cases

| Persona | Use |
|---|---|
| Distribution manager | Onboard channels, manage mappings/markups, push ARI |
| Revenue manager | Configure controls and per-channel windows |
| Reservations ops | Monitor received bookings, replay syncs |
| Channel partner (integration) | Submit bookings, consume ARI feeds |
| Hotel admin | Policy admin over distribution |

## 7. KPIs / Success Criteria

- ARI propagation completes within the channel-contract latency.
- Zero "unmapped rate sold" incidents.
- Sync ledger: > 0 retries visible and manageable; DLQ empty steady-state.
- Booking intake → PMS reservation measured and under budget.

## 8. Dependencies & Reliance on Other Services

| Reliance | For |
|---|---|
| hotel8:pms | Inventory truth, reservation creation for channel bookings |
| hotel8:meta | Channel credentials/keys |
| Sentin8:Auth + Shield | Identity, ARI edit windows (ABAC), partner edges (ReBAC) |
| Sentin8:Bus | `distrib` stream + PMS events intake |
| Sentin8:Sys | Audit ingestion |

## 9. Acceptance Criteria

1. A new channel can be onboarded, mapped, and selling without code change.
2. A PMS inventory change triggers per-mapping ARI pushes automatically.
3. A channel booking lands in PMS as a first-class reservation with channel
   attribution intact.
4. A failed push replays from the ledger without data loss.
5. Channel A cannot see or push channel B's inventory.

## 10. Open Business Questions

- Two-way content sync (pulling channel-side updates back).
- Rate-parity monitoring across channels.

---

**Design attribution:** hotel8:distrib and its underlying concepts were designed by Hamid Jahanzad <Hamid@Jahanzad.com>.
