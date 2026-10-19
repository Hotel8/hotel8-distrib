// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Hotel8:Distrib
// SPDX-License-Identifier: Apache-2.0

//! Pure distrib sync state - DB-free, testable.
//!
//! `SyncState` over the `hotel8.distrib_sync_status` DB enum labels
//! (PENDING/SYNCED/FAILED/RETRY - see `services/distrib/sql/init.sql`), the
//! retry-eligibility rule from `retry_failed_syncs` (FAILED + window elapsed),
//! and the channel markup/markdown price math.

/// Sync statuses (DB enum `hotel8.distrib_sync_status` labels).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncState {
    Pending,
    Synced,
    Failed,
    Retry,
}

impl SyncState {
    pub fn label(self) -> &'static str {
        match self {
            SyncState::Pending => "PENDING",
            SyncState::Synced => "SYNCED",
            SyncState::Failed => "FAILED",
            SyncState::Retry => "RETRY",
        }
    }

    pub fn from_label(s: &str) -> Self {
        match s {
            "SYNCED" => SyncState::Synced,
            "FAILED" => SyncState::Failed,
            "RETRY" => SyncState::Retry,
            _ => SyncState::Pending,
        }
    }
}

/// Retry-eligibility rule (mirrors `retry_failed_syncs`' WHERE clause):
/// a FAILED update is retryable once its retry window has elapsed.
pub fn is_retry_eligible(state: SyncState, last_attempt_hours_ago: f64, window_hours: f64) -> bool {
    state == SyncState::Failed && last_attempt_hours_ago >= window_hours
}

/// Channel price math from `push_ari`: markup then markdown, 2-dp rounded.
pub fn marked_up_price(amount: f64, markup_pct: f64, markdown_pct: f64) -> f64 {
    let p = amount * (1.0 + markup_pct / 100.0) * (1.0 - markdown_pct / 100.0);
    (p * 100.0).round() / 100.0
}

/// Channel booking status → PMS reservation status mapping
/// (`receive_reservation` / `update_channel_reservation` semantics).
pub fn booking_status_to_pms(booking_status: &str) -> Option<&'static str> {
    match booking_status {
        "NEW" | "DELIVERED" => Some("CONFIRMED"),
        "CANCELLED" => Some("CANCELLED"),
        "MODIFIED" => Some("CONFIRMED"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use SyncState::*;

    #[test]
    fn labels_match_db_enums() {
        assert_eq!(Pending.label(), "PENDING");
        assert_eq!(Synced.label(), "SYNCED");
        assert_eq!(Failed.label(), "FAILED");
        assert_eq!(Retry.label(), "RETRY");
    }

    #[test]
    fn from_label_round_trips() {
        for t in [Pending, Synced, Failed, Retry] {
            assert_eq!(SyncState::from_label(t.label()), t);
        }
        assert_eq!(SyncState::from_label("garbage"), Pending);
    }

    #[test]
    fn retry_eligibility_window() {
        assert!(is_retry_eligible(Failed, 2.0, 1.0));
        assert!(!is_retry_eligible(Failed, 0.5, 1.0));
        assert!(!is_retry_eligible(Pending, 5.0, 1.0));
        assert!(!is_retry_eligible(Synced, 5.0, 1.0));
    }

    #[test]
    fn markup_then_markdown_math() {
        assert_eq!(marked_up_price(100.0, 10.0, 0.0), 110.0);
        assert_eq!(marked_up_price(100.0, 0.0, 10.0), 90.0);
        assert_eq!(marked_up_price(100.0, 10.0, 10.0), 99.0);
        assert_eq!(marked_up_price(33.335, 0.0, 0.0), 33.34); // 2dp rounding
    }

    #[test]
    fn booking_status_mapping() {
        assert_eq!(booking_status_to_pms("NEW"), Some("CONFIRMED"));
        assert_eq!(booking_status_to_pms("DELIVERED"), Some("CONFIRMED"));
        assert_eq!(booking_status_to_pms("CANCELLED"), Some("CANCELLED"));
        assert_eq!(booking_status_to_pms("garbage"), None);
    }
}
