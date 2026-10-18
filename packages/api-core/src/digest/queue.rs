//! Queue payloads for the digest jobs, and the discriminant that keeps the
//! existing calendar-sync JSON working.
//!
//! The `calendar-sync` consumer used to carry a bare
//! [`CalendarSyncMessage`] (`{"calendar_id":"…"}`). Reports adds a second
//! payload on the same binding, so the batch type becomes
//! [`WorkerQueueMessage`] — an **untagged** enum whose first variant is
//! `CalendarSync`. Serde's untagged matching tries the variants in order, so a
//! calendar payload can never be mistaken for a digest payload (and vice
//! versa: a digest payload has no `calendar_id`, so it fails the first
//! variant).
//!
//! The payload-compatibility test lives here, in `api-core`, because the
//! Worker crate is `cdylib`/wasm-only and its unit tests do not run on the
//! host — `cargo test -p api-core` is the only place that can assert the wire
//! format.

use serde::{Deserialize, Serialize};

use crate::calendar::CalendarSyncMessage;

/// Which digest job a [`DigestQueueMessage`] asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DigestJobKind {
    /// Rebuild the user's dirty days (and drive a running backfill).
    Rebuild,
    /// Re-run the classifier over the user's cached events.
    Reclassify,
}

/// A digest job request: `{"kind":"rebuild","user_id":"u-1"}`.
///
/// `user_id` is deliberately NOT `#[serde(default)]` — a payload without a
/// user is a producer bug and must fail to deserialize rather than silently
/// no-op.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestQueueMessage {
    pub kind: DigestJobKind,
    pub user_id: String,
}

/// Every payload that can arrive on the `CALENDAR_SYNC` binding.
///
/// The `CalendarSync` variant is **first** so `{"calendar_id":"…"}` still
/// deserializes exactly as it always did.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum WorkerQueueMessage {
    CalendarSync(CalendarSyncMessage),
    Digest(DigestQueueMessage),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_calendar_payload_still_wins() {
        let payload = r#"{"calendar_id":"cal-1"}"#;
        match serde_json::from_str::<WorkerQueueMessage>(payload).expect("calendar payload") {
            WorkerQueueMessage::CalendarSync(message) => {
                assert_eq!(message.calendar_id, "cal-1");
            }
            WorkerQueueMessage::Digest(_) => panic!("calendar payload must not match Digest"),
        }
    }

    #[test]
    fn calendar_payload_with_extra_keys_still_matches() {
        // Serde ignores unknown fields by default — a producer that adds a
        // field must not break the consumer.
        let payload = r#"{"calendar_id":"cal-2","reason":"webhook"}"#;
        match serde_json::from_str::<WorkerQueueMessage>(payload).expect("calendar payload") {
            WorkerQueueMessage::CalendarSync(message) => {
                assert_eq!(message.calendar_id, "cal-2");
            }
            WorkerQueueMessage::Digest(_) => panic!("calendar payload must not match Digest"),
        }
    }

    #[test]
    fn digest_rebuild_payload_round_trips() {
        let payload = r#"{"kind":"rebuild","user_id":"u-1"}"#;
        let message: WorkerQueueMessage =
            serde_json::from_str(payload).expect("digest payload");
        match message {
            WorkerQueueMessage::Digest(message) => {
                assert_eq!(
                    message,
                    DigestQueueMessage {
                        kind: DigestJobKind::Rebuild,
                        user_id: "u-1".to_string(),
                    }
                );
                assert_eq!(
                    serde_json::to_string(&message).expect("serialize"),
                    payload,
                    "the producer and consumer shapes must agree"
                );
            }
            WorkerQueueMessage::CalendarSync(_) => panic!("digest payload must not match CalendarSync"),
        }
    }

    #[test]
    fn digest_reclassify_payload_round_trips() {
        let message = DigestQueueMessage {
            kind: DigestJobKind::Reclassify,
            user_id: "u-2".to_string(),
        };
        let json = serde_json::to_string(&message).expect("serialize");
        assert_eq!(json, r#"{"kind":"reclassify","user_id":"u-2"}"#);
        assert_eq!(
            serde_json::from_str::<DigestQueueMessage>(&json).expect("deserialize"),
            message
        );
    }

    #[test]
    fn digest_payload_without_user_fails() {
        // No `#[serde(default)]`: a missing user must be a deserialize error.
        assert!(serde_json::from_str::<DigestQueueMessage>(r#"{"kind":"rebuild"}"#).is_err());
        assert!(serde_json::from_str::<WorkerQueueMessage>(r#"{"kind":"rebuild"}"#).is_err());
    }

    #[test]
    fn unknown_kind_fails() {
        assert!(
            serde_json::from_str::<DigestQueueMessage>(r#"{"kind":"reindex","user_id":"u-1"}"#)
                .is_err()
        );
    }
}
