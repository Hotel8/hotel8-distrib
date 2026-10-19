-- SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
-- SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
-- SPDX-FileCopyrightText: Copyright Contributors to the Hotel8:Distrib
-- SPDX-License-Identifier: Apache-2.0

-- Hotel8:Distribution - Distribution service tables (channel manager).
CREATE SCHEMA IF NOT EXISTS distrib;
DO $$ BEGIN
    CREATE TYPE distrib.channel_type AS ENUM ('OTA', 'GDS', 'DIRECT', 'WHOLESALER', 'CORPORATE', 'METASEARCH');
EXCEPTION WHEN duplicate_object THEN NULL; END $$;

DO $$ BEGIN
    CREATE TYPE distrib.sync_status AS ENUM ('PENDING', 'SYNCED', 'FAILED', 'RETRY');
EXCEPTION WHEN duplicate_object THEN NULL; END $$;

CREATE TABLE IF NOT EXISTS distrib.channels (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    property_id  UUID NOT NULL,
    env_id       UUID NOT NULL,
    channel_code TEXT NOT NULL,
    channel_name TEXT NOT NULL,
    channel_type distrib.channel_type NOT NULL DEFAULT 'OTA',
    is_active    BOOLEAN NOT NULL DEFAULT true,
    api_endpoint TEXT,
    api_key_ref  TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_distrib_channels_property ON distrib.channels (property_id);
CREATE INDEX IF NOT EXISTS idx_distrib_channels_env ON distrib.channels (env_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_distrib_channels_code ON distrib.channels (property_id, channel_code);

CREATE TABLE IF NOT EXISTS distrib.rate_updates (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    channel_id    UUID NOT NULL REFERENCES distrib.channels(id),
    room_type_id  UUID,
    rate_plan_code TEXT,
    start_date    DATE NOT NULL,
    end_date      DATE NOT NULL,
    rate_amount   NUMERIC(12,2) NOT NULL DEFAULT 0,
    currency_code TEXT NOT NULL DEFAULT 'USD',
    sync_status   distrib.sync_status NOT NULL DEFAULT 'PENDING',
    synced_at     TIMESTAMPTZ,
    error_message TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_distrib_ru_channel ON distrib.rate_updates (channel_id);
CREATE INDEX IF NOT EXISTS idx_distrib_ru_sync ON distrib.rate_updates (sync_status);

CREATE TABLE IF NOT EXISTS distrib.inventory_updates (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    channel_id    UUID NOT NULL REFERENCES distrib.channels(id),
    room_type_id  UUID,
    date          DATE NOT NULL,
    available     INTEGER NOT NULL DEFAULT 0,
    sync_status   distrib.sync_status NOT NULL DEFAULT 'PENDING',
    synced_at     TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_distrib_iu_channel ON distrib.inventory_updates (channel_id);

CREATE TABLE IF NOT EXISTS distrib.bookings (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    channel_id     UUID NOT NULL REFERENCES distrib.channels(id),
    property_id    UUID NOT NULL,
    env_id         UUID NOT NULL,
    channel_booking_id TEXT NOT NULL,
    guest_name     TEXT,
    arrival_date    DATE NOT NULL,
    departure_date  DATE NOT NULL,
    num_rooms       INTEGER NOT NULL DEFAULT 1,
    total_amount    NUMERIC(12,2) NOT NULL DEFAULT 0,
    currency_code   TEXT NOT NULL DEFAULT 'USD',
    status          TEXT NOT NULL DEFAULT 'NEW',
    reservation_id  UUID,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_distrib_bookings_channel ON distrib.bookings (channel_id);
CREATE INDEX IF NOT EXISTS idx_distrib_bookings_property ON distrib.bookings (property_id);

ALTER TABLE distrib.channels ENABLE ROW LEVEL SECURITY;
ALTER TABLE distrib.channels FORCE ROW LEVEL SECURITY;
ALTER TABLE distrib.rate_updates ENABLE ROW LEVEL SECURITY;
ALTER TABLE distrib.rate_updates FORCE ROW LEVEL SECURITY;
ALTER TABLE distrib.inventory_updates ENABLE ROW LEVEL SECURITY;
ALTER TABLE distrib.inventory_updates FORCE ROW LEVEL SECURITY;
ALTER TABLE distrib.bookings ENABLE ROW LEVEL SECURITY;
ALTER TABLE distrib.bookings FORCE ROW LEVEL SECURITY;
-- env_id is the sole tenant key (architecture-principles.md §4.3).
DROP POLICY IF EXISTS distrib_channels_rls ON distrib.channels;
CREATE POLICY distrib_channels_rls ON distrib.channels USING (sentin8_context_initialized() AND env_id = sentin8_current_env_id());
DROP POLICY IF EXISTS distrib_ru_rls ON distrib.rate_updates;
CREATE POLICY distrib_ru_rls ON distrib.rate_updates USING (channel_id IN (SELECT id FROM distrib.channels));
DROP POLICY IF EXISTS distrib_iu_rls ON distrib.inventory_updates;
CREATE POLICY distrib_iu_rls ON distrib.inventory_updates USING (channel_id IN (SELECT id FROM distrib.channels));
DROP POLICY IF EXISTS distrib_bookings_rls ON distrib.bookings;
CREATE POLICY distrib_bookings_rls ON distrib.bookings USING (sentin8_context_initialized() AND env_id = sentin8_current_env_id());
-- ──────────────────────────────────────────────────────────────────────────
-- Local outbox (DBX, architecture-principles §6.7) - producer path:
-- rows are appended in the SAME transaction as the domain change; the
-- shell's post-commit relay pushes them to Bus POST /v1/produce
-- (M2M sentin8:bus:produce, idempotency_key = row id).
-- Generated from sentin8_outbox::outbox_table_sql("distrib").
-- ──────────────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS distrib.outbox (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id          UUID NOT NULL,
    event_type      TEXT NOT NULL,
    stream_family   TEXT NOT NULL,
    payload         JSONB NOT NULL,
    publisher_id_token  TEXT,
    publisher_subject   TEXT,
    relayed         BOOLEAN NOT NULL DEFAULT false,
    relayed_at      TIMESTAMPTZ,
    relay_attempts  INT NOT NULL DEFAULT 0,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_outbox_unrelayed
    ON distrib.outbox (created_at) WHERE relayed = false;
ALTER TABLE distrib.outbox ENABLE ROW LEVEL SECURITY;
ALTER TABLE distrib.outbox FORCE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS outbox_select ON distrib.outbox;
CREATE POLICY outbox_select ON distrib.outbox FOR SELECT
    USING (sentin8_context_initialized() AND env_id = sentin8_current_env_id());
DROP POLICY IF EXISTS outbox_insert ON distrib.outbox;
CREATE POLICY outbox_insert ON distrib.outbox FOR INSERT
    WITH CHECK (sentin8_context_initialized() AND env_id = sentin8_current_env_id());
DROP POLICY IF EXISTS outbox_update ON distrib.outbox;
CREATE POLICY outbox_update ON distrib.outbox FOR UPDATE
    USING (sentin8_context_initialized() AND env_id = sentin8_current_env_id());
CREATE OR REPLACE FUNCTION distrib.distrib_notify_outbox() RETURNS TRIGGER
    LANGUAGE plpgsql AS $fn$
BEGIN
    PERFORM pg_notify('distrib_outbox_changed', '');
    RETURN NEW;
END;
$fn$;
DROP TRIGGER IF EXISTS outbox_notify_trigger ON distrib.outbox;
CREATE TRIGGER outbox_notify_trigger
    AFTER INSERT ON distrib.outbox
    FOR EACH ROW
    EXECUTE FUNCTION distrib.distrib_notify_outbox();

-- ──────────────────────────────────────────────────────────────────────────
-- Local audit journal (architecture-principles §6.8): same-transaction
-- journal rows; the shell's post-commit pusher ships them to Sys
-- POST /v1/audit (M2M sentin8:sys:audit:write). A Sys outage only delays
-- delivery - rows persist until pushed.
-- Generated from sentin8_audit::audit_journal_sql("distrib").
-- ──────────────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS distrib.audit_journal (
    id              UUID PRIMARY KEY,
    env_id          UUID NOT NULL,
    action          TEXT NOT NULL,
    target_resource_type TEXT,
    target_resource_id    TEXT,
    actor_user_id   TEXT,
    actor_subject_type TEXT,
    severity        TEXT NOT NULL DEFAULT 'info',
    correlation_id  TEXT,
    before_state    JSONB,
    after_state     JSONB,
    details         JSONB,
    pushed          BOOLEAN NOT NULL DEFAULT false,
    pushed_at       TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_distrib_audit_journal_unpushed
    ON distrib.audit_journal (created_at) WHERE pushed = false;
ALTER TABLE distrib.audit_journal ENABLE ROW LEVEL SECURITY;
ALTER TABLE distrib.audit_journal FORCE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS audit_journal_dml ON distrib.audit_journal;
CREATE POLICY audit_journal_dml ON distrib.audit_journal
    USING (sentin8_context_initialized() AND env_id = sentin8_current_env_id())
    WITH CHECK (sentin8_context_initialized() AND env_id = sentin8_current_env_id());

-- ──────────────────────────────────────────────────────────────────────────
-- Per-service role grants + search_path (§4.3e / VDC §VDC.3):
-- hotel8_{leaf}_service connects via PostgreSQL 18 OAuth (scope→role map in
-- hotel8.sys.pg_oauth_scope_map). search_path puts the service's OWN schema
-- FIRST (outbox/audit_journal resolution for the shell's relay + journal
-- pusher; unqualified domain tables resolve here), then public (RLS helpers).
-- `sentin8-cli vertical enroll`'s never-clobber block preserves this role-level setting.
-- ──────────────────────────────────────────────────────────────────────────
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'hotel8_distrib_service') THEN
        ALTER ROLE hotel8_distrib_service SET search_path = distrib, public;
        GRANT USAGE ON SCHEMA distrib TO hotel8_distrib_service;
        GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA distrib TO hotel8_distrib_service;
        GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA distrib TO hotel8_distrib_service;
    END IF;
END $$;


-- ═════════════════════════════════════════════════════════════════════════════
-- From migrations: extended
-- ═══════════════════════════════════════════════════════════════════════════
-- Migration 32: Hotel8:Distribution extended tables for channel mappings and controls
-- H3.2.2-H3.2.6: Channel property mapping, room type/rate plan mapping, rate markup, controls
-- H3.3.x: ARI sync tracking
-- H3.5.x: Channel-facing content

CREATE SCHEMA IF NOT EXISTS distrib;
CREATE TABLE IF NOT EXISTS distrib.channel_properties (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    channel_id      UUID NOT NULL REFERENCES distrib.channels(id),
    property_id     UUID NOT NULL,
    env_id          UUID NOT NULL,
    is_enabled      BOOLEAN NOT NULL DEFAULT true,
    rate_markup_pct NUMERIC(5,2) NOT NULL DEFAULT 0,
    rate_markdown_pct NUMERIC(5,2) NOT NULL DEFAULT 0,
    cta_enabled     BOOLEAN NOT NULL DEFAULT true,
    ctd_enabled     BOOLEAN NOT NULL DEFAULT true,
    min_los         INTEGER,
    max_los         INTEGER,
    last_synced_at  TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_distrib_cp_channel_prop ON distrib.channel_properties (channel_id, property_id);
CREATE INDEX IF NOT EXISTS idx_distrib_cp_property ON distrib.channel_properties (property_id);

CREATE TABLE IF NOT EXISTS distrib.room_type_mappings (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    channel_property_id UUID NOT NULL REFERENCES distrib.channel_properties(id),
    property_room_type_id UUID NOT NULL,
    channel_room_type_code TEXT NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_distrib_rtm_cp_rt ON distrib.room_type_mappings (channel_property_id, property_room_type_id);

CREATE TABLE IF NOT EXISTS distrib.rate_plan_mappings (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    channel_property_id UUID NOT NULL REFERENCES distrib.channel_properties(id),
    property_rate_plan_id UUID NOT NULL,
    channel_rate_plan_code TEXT NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_distrib_rpm_cp_rp ON distrib.rate_plan_mappings (channel_property_id, property_rate_plan_id);

CREATE TABLE IF NOT EXISTS distrib.sync_log (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    channel_id      UUID NOT NULL REFERENCES distrib.channels(id),
    property_id     UUID,
    sync_type       TEXT NOT NULL DEFAULT 'ARI',
    status          TEXT NOT NULL DEFAULT 'PENDING',
    attempts        INTEGER NOT NULL DEFAULT 0,
    next_retry_at   TIMESTAMPTZ,
    error_message   TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at    TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS idx_distrib_sl_channel ON distrib.sync_log (channel_id);
CREATE INDEX IF NOT EXISTS idx_distrib_sl_status ON distrib.sync_log (status);

ALTER TABLE distrib.channel_properties ENABLE ROW LEVEL SECURITY;
ALTER TABLE distrib.channel_properties FORCE ROW LEVEL SECURITY;
ALTER TABLE distrib.room_type_mappings ENABLE ROW LEVEL SECURITY;
ALTER TABLE distrib.room_type_mappings FORCE ROW LEVEL SECURITY;
ALTER TABLE distrib.rate_plan_mappings ENABLE ROW LEVEL SECURITY;
ALTER TABLE distrib.rate_plan_mappings FORCE ROW LEVEL SECURITY;
ALTER TABLE distrib.sync_log ENABLE ROW LEVEL SECURITY;
ALTER TABLE distrib.sync_log FORCE ROW LEVEL SECURITY;
CREATE POLICY distrib_cp_rls ON distrib.channel_properties USING (sentin8_context_initialized() AND env_id = sentin8_current_env_id());
CREATE POLICY distrib_rtm_rls ON distrib.room_type_mappings USING (channel_property_id IN (SELECT id FROM distrib.channel_properties));
CREATE POLICY distrib_rpm_rls ON distrib.rate_plan_mappings USING (channel_property_id IN (SELECT id FROM distrib.channel_properties));
CREATE POLICY distrib_sl_rls ON distrib.sync_log USING (channel_id IN (SELECT id FROM distrib.channels));
