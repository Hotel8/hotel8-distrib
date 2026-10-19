// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Hotel8:Distrib
// SPDX-License-Identifier: Apache-2.0

//! Axum server setup for Distrib.
use crate::handlers::AppState;
use axum::routing::{get, post, put};
use std::sync::Arc;

pub async fn build_state(
    core: sentin8::shell::ServiceCore,
) -> anyhow::Result<crate::handlers::AppState> {
    let config = crate::config::DistribConfig::from_env();

    // Cert-presenting Bus client (CERTFREE-4V): the `distrib` workload
    // CERTIFICATE is the identity - no DCR, no token (auth DESIGN
    // §4.3f); the mint retries while the fleet boots.
    let bus_client = loop {
        match sentin8::bus::client::BusClient::for_workload(
            &config.sentin8_bus_url,
            &config.sentin8_auth_issuer,
            "distrib",
        )
        .await
        {
            Ok(c) => break c,
            Err(e) => {
                tracing::debug!(error = %e, "distrib bus client: certificate not ready yet");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    };

    // PMS cross-service client (CERTFREE-4V): this service presents its
    // own `distrib` workload certificate - PMS's TLS layer authenticates
    // the handshake and the registry map authorizes the scopes (the
    // `pms:reservations:read` / `pms:profiles:read` grants ride the
    // `distrib` map row). The mint retries while the fleet boots.
    let pms_client = loop {
        match hotel8::pms::client::H8pmsClient::with_workload_certificate(
            &config.pms_url,
            &config.sentin8_auth_issuer,
            "distrib",
        )
        .await
        {
            Ok(c) => break c,
            Err(e) => {
                tracing::debug!(error = %e, "distrib pms client: certificate not ready yet");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    };

    // ── Sentin8:Policy (Shield) ──
    // Load the service's policy sets; an unparsable policy text REFUSES TO
    // START (fail-closed at boot, design §3.3). Hot-reload consumes the
    // `policy` stream as a direct consumer (design §3.2); deny/would-deny
    // decisions emit to Stat (§4.2f). The engine is stored in AppState so
    // handlers gate via `policy::require` (step 5 is the ONLY gate).
    // ── Sentin8:Policy (Shield, shell-owned boot): consume the
    // engine the shell booted in step 8 - no second engine, consumer, or
    // DCR registration here.
    let policy_engine = core.admin.policy.clone();

    let state = AppState {
        config: Arc::new(config),
        pool: core.pool.clone(),
        bus_client,
        pms_client,
        policy: policy_engine.clone(),
        admin: sentin8::shield::admin::SharedAdminState {
            service: "hotel8:distrib".to_string(),
            pool: core.pool.clone(),
            policy: policy_engine,
            auth_client: Arc::new(core.config.sentin8_auth_client.clone()),
        },
    };

    // Event-driven consumer - subscribes to the PMS stream family.
    crate::event_consumer::spawn(core.pool.clone(), state.bus_client.clone());

    Ok(state)
}

/// Add this service's routes onto the shell-provided router. The shell
/// owns the boot sequence, state binding, OpenAPI merge and the standard
/// middleware stack; this adds ONLY routes (including the service's own
/// `/v1/health` - custom payload).
pub fn routes(
    router: axum::Router<crate::handlers::AppState>,
) -> axum::Router<crate::handlers::AppState> {
    router
        .route("/v1/health", get(crate::handlers::health))
        // ── Admin UI (RFW) ─────────────────────────────────────────────────
        .route("/v1/admin/ui/{*page}", get(crate::admin_ui::get_admin_ui))
        // ── Sentin8:Policy admin API (shared implementation,
        // handlers generic over this AppState).
        .route(
            "/v1/admin/policy-sets",
            get(sentin8::shield::admin::list_policy_sets::<crate::handlers::AppState>),
        )
        .route(
            "/v1/admin/policy-sets/{name}",
            get(sentin8::shield::admin::get_policy_set::<crate::handlers::AppState>)
                .put(sentin8::shield::admin::put_policy_set::<crate::handlers::AppState>)
                .delete(sentin8::shield::admin::delete_policy_set::<crate::handlers::AppState>),
        )
        .route(
            "/v1/admin/policy-sets/{name}/test",
            post(sentin8::shield::admin::test_policy_set::<crate::handlers::AppState>),
        )
        .route(
            "/v1/admin/policy-sets/enforce",
            post(sentin8::shield::admin::admin_enforce_policy_set::<crate::handlers::AppState>),
        )
        .route(
            "/v1/admin/policy-sets/shadow",
            post(sentin8::shield::admin::admin_shadow_policy_set::<crate::handlers::AppState>),
        )
        .route(
            "/v1/admin/policy-sets/disable",
            post(sentin8::shield::admin::admin_disable_policy_set::<crate::handlers::AppState>),
        )
        .route(
            "/v1/admin/relations",
            get(sentin8::shield::admin::list_relations::<crate::handlers::AppState>)
                .put(sentin8::shield::admin::put_relation::<crate::handlers::AppState>)
                .delete(sentin8::shield::admin::delete_relation::<crate::handlers::AppState>),
        )
        .route(
            "/v1/admin/relations/check",
            get(sentin8::shield::admin::check_relation::<crate::handlers::AppState>),
        )
        .route(
            "/v1/admin/attributes",
            get(sentin8::shield::admin::list_attributes::<crate::handlers::AppState>)
                .put(sentin8::shield::admin::put_attribute::<crate::handlers::AppState>)
                .delete(sentin8::shield::admin::delete_attribute::<crate::handlers::AppState>),
        )
        .route(
            "/v1/channels",
            post(crate::handlers::create_channel).get(crate::handlers::list_channels),
        )
        // Channel CRUD
        .route(
            "/v1/channels/{channel_id}",
            put(crate::handlers::update_channel).delete(crate::handlers::delete_channel),
        )
        // Channel Property mapping
        .route(
            "/v1/channel-properties",
            post(crate::handlers::map_channel_property),
        )
        .route(
            "/v1/channels/{channel_id}/mapped-properties",
            get(crate::handlers::list_channel_properties),
        )
        // Room Type mapping
        .route(
            "/v1/room-type-mappings",
            post(crate::handlers::map_room_type),
        )
        // Rate Plan mapping
        .route(
            "/v1/rate-plan-mappings",
            post(crate::handlers::map_rate_plan),
        )
        // Rate markup
        .route(
            "/v1/channel-properties/{channel_property_id}/markup",
            put(crate::handlers::set_rate_markup),
        )
        // Property controls
        .route(
            "/v1/channel-properties/{channel_property_id}/controls",
            put(crate::handlers::set_property_controls),
        )
        // ARI Publication
        .route(
            "/v1/channels/{channel_id}/ari/push",
            post(crate::handlers::push_ari),
        )
        .route(
            "/v1/channels/{channel_id}/content/push",
            post(crate::handlers::push_content),
        )
        .route(
            "/v1/channels/{channel_id}/controls/push",
            post(crate::handlers::push_controls),
        )
        .route("/v1/sync/retry", post(crate::handlers::retry_failed_syncs))
        // Reservation Delivery
        .route(
            "/v1/channels/{channel_code}/reservations",
            post(crate::handlers::receive_reservation),
        )
        .route(
            "/v1/channels/{channel_code}/reservations/{booking_id}",
            put(crate::handlers::update_channel_reservation),
        )
        .route(
            "/v1/channels/{channel_code}/reservations/{booking_id}/status",
            post(crate::handlers::push_reservation_status),
        )
        // Channel-Facing API
        .route(
            "/v1/channels/{channel_code}/properties",
            get(crate::handlers::get_channel_properties),
        )
        .route(
            "/v1/channels/{channel_code}/properties/{property_code}/content",
            get(crate::handlers::get_channel_property_content),
        )
        .route(
            "/v1/channels/{channel_code}/properties/{property_code}/ari",
            get(crate::handlers::get_channel_ari),
        )
        .route(
            "/v1/channels/{channel_code}/properties/{property_code}/controls",
            get(crate::handlers::get_channel_controls),
        )
        // Hardening
        .route(
            "/v1/channels/{channel_code}/validate",
            get(crate::handlers::validate_channel_credentials),
        )
        .route("/v1/sync-log", get(crate::handlers::get_sync_log))
}

/// Shared policy-admin surface accessor - the shared
/// handlers extract this service's admin state via `HasPolicyAdmin`.
impl sentin8::shield::admin::HasPolicyAdmin for crate::handlers::AppState {
    fn policy_admin(&self) -> &sentin8::shield::admin::SharedAdminState {
        &self.admin
    }
}
