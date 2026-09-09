//! Wire protocol for user-hub realtime hints.
//! JSON only. No Worker / Durable Object types.

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
