// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Hotel8:Distrib
// SPDX-License-Identifier: Apache-2.0

//! Library re-exports for integration tests.
pub mod admin_ui;
pub mod auth;
pub mod config;
pub mod event_consumer;
pub mod event_emit;
pub mod handlers;
pub mod openapi;

// The utoipa dump for registration.rs - mirrored in main.rs for the binary;
// the lib crate compiles registration::print too (the lib is also used by
// test harnesses that never invoke print, so a null dump is unreachable there).
#[cfg_attr(test, allow(dead_code))]
static OPENAPI_DUMP: fn() -> serde_json::Value = || {
    use utoipa::OpenApi as _;
    serde_json::to_value(openapi::DistribOpenApi::openapi()).unwrap_or(serde_json::Value::Null)
};

pub mod policy;
pub mod registration;
pub mod server;
pub mod sync_state;
