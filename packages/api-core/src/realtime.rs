//! Wire protocol for user-hub realtime hints.
//! JSON only. No Worker / Durable Object types.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealtimeMessage {
    #[serde(rename = "type")]
    pub kind: RealtimeKind,
    /// Owning local calendar id when the change is scoped. Omit when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RealtimeKind {
    #[serde(rename = "calendar.changed")]
    CalendarChanged,
}

impl RealtimeMessage {
    pub fn calendar_changed(calendar_id: Option<String>) -> Self {
        Self {
            kind: RealtimeKind::CalendarChanged,
            calendar_id,
        }
    }
}

/// Fan-out port for `calendar.changed`.
///
/// `#[async_trait(?Send)]` because this must be `&dyn`-compatible and the
/// Worker adapter awaits `notify_user` (`worker::Fetch` futures are `!Send`
/// on wasm). Same reason as [`crate::oauth::HttpClient`].
///
/// The method returns `()` on purpose: a realtime hint must never be able to
/// fail a write. The Worker adapter calls `crate::user_hub::notify_user`,
/// which already swallows every error.
#[async_trait(?Send)]
pub trait RealtimeNotify: Send + Sync {
    /// Tells every open client of `user_id` that its calendar view is stale.
    /// `calendar_id` is the **local** calendar id when the change is scoped.
    async fn calendar_changed(&self, user_id: &str, calendar_id: Option<&str>);
}

#[cfg(test)]
pub(crate) struct NoopNotify;

#[cfg(test)]
#[async_trait(?Send)]
impl RealtimeNotify for NoopNotify {
    async fn calendar_changed(&self, _user_id: &str, _calendar_id: Option<&str>) {}
}

#[cfg(test)]
pub(crate) fn noop_notify() -> &'static dyn RealtimeNotify {
    static NOOP: NoopNotify = NoopNotify;
    &NOOP
}

/// Records every `calendar_changed` call as `(user_id, calendar_id)`.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct RecordingNotify {
    pub(crate) calls: std::sync::Mutex<Vec<(String, Option<String>)>>,
}

#[cfg(test)]
impl RecordingNotify {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
#[async_trait(?Send)]
impl RealtimeNotify for RecordingNotify {
    async fn calendar_changed(&self, user_id: &str, calendar_id: Option<&str>) {
        self.calls
            .lock()
            .unwrap()
            .push((user_id.to_string(), calendar_id.map(str::to_string)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_changed_with_id_round_trips() {
        let msg = RealtimeMessage::calendar_changed(Some("cal-1".to_string()));
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"type":"calendar.changed","calendar_id":"cal-1"}"#);

        let parsed: RealtimeMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, msg);
        assert_eq!(parsed.kind, RealtimeKind::CalendarChanged);
        assert_eq!(parsed.calendar_id.as_deref(), Some("cal-1"));
    }

    #[test]
    fn calendar_changed_without_id_omits_key() {
        let msg = RealtimeMessage::calendar_changed(None);
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"type":"calendar.changed"}"#);

        let parsed: RealtimeMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, msg);
        assert!(parsed.calendar_id.is_none());
    }

    #[test]
    fn unknown_type_fails_deserialize() {
        let err = serde_json::from_str::<RealtimeMessage>(r#"{"type":"unknown.event"}"#);
        assert!(err.is_err());
    }
}
