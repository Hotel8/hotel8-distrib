// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Hotel8:Distrib
// SPDX-License-Identifier: Apache-2.0

//! Hotel8:Distrib integration tests (test-strategy Layer 3) — DB-free.
//!
//! Exercises the extracted sync state, channel price math and booking-status
//! mapping exactly as `push_ari` / `retry_failed_syncs` /
//! `receive_reservation` apply them.

use hotel8_distrib_server::sync_state::{self as sync, SyncState};
use sync::SyncState::*;

#[test]
fn sync_labels_match_db_enum() {
    assert_eq!(Pending.label(), "PENDING");
    assert_eq!(Synced.label(), "SYNCED");
    assert_eq!(Failed.label(), "FAILED");
    assert_eq!(Retry.label(), "RETRY");
    for t in [Pending, Synced, Failed, Retry] {
        assert_eq!(SyncState::from_label(t.label()), t);
    }
}

#[test]
fn retry_window_semantics() {
    // retry_failed_syncs: FAILED + window elapsed → RETRY.
    assert!(sync::is_retry_eligible(Failed, 1.0, 1.0));
    assert!(sync::is_retry_eligible(Failed, 100.0, 1.0));
    assert!(!sync::is_retry_eligible(Failed, 0.99, 1.0));
    // Non-failed states are never retryable regardless of age.
    for t in [Pending, Synced, Retry] {
        assert!(!sync::is_retry_eligible(t, 999.0, 1.0));
    }
}

#[test]
fn channel_price_math_ari_pipeline() {
    // push_ari: sell_limit → markup → markdown, 2dp.
    assert_eq!(sync::marked_up_price(120.0, 15.0, 0.0), 138.0);
    assert_eq!(sync::marked_up_price(120.0, 0.0, 15.0), 102.0);
    assert_eq!(sync::marked_up_price(120.0, 15.0, 15.0), 117.30);
    // Zero-rate inventory rows stay zero.
    assert_eq!(sync::marked_up_price(0.0, 25.0, 10.0), 0.0);
}

#[test]
fn booking_status_mapping_delivery_flow() {
    assert_eq!(sync::booking_status_to_pms("NEW"), Some("CONFIRMED"));
    assert_eq!(sync::booking_status_to_pms("DELIVERED"), Some("CONFIRMED"));
    assert_eq!(sync::booking_status_to_pms("MODIFIED"), Some("CONFIRMED"));
    assert_eq!(sync::booking_status_to_pms("CANCELLED"), Some("CANCELLED"));
    assert_eq!(sync::booking_status_to_pms("UNKNOWN"), None);
}

#[test]
fn round_trip_after_retry_label_write() {
    // The retry UPDATE writes 'RETRY' — reading it back parses to Retry.
    assert_eq!(SyncState::from_label("RETRY"), Retry);
}