use super::google::encode_path_segment;
use super::{CalendarError, GOOGLE_EVENTS_BASE_URL};
use crate::google_color::canonicalize_hex;
use crate::models::GoogleCalendar;
use crate::oauth::HttpClient;
use crate::repo::CalendarRepo;
use crate::token::GoogleAccess;
use serde::{Deserialize, Serialize};

/// Subset of the `calendars.get` response: the label properties carrying the
/// calendar's event labels (`labelProperties.eventLabels[]`, each
/// `{id, backgroundColor}` — `name` is often null and is ignored).
#[derive(Debug, Deserialize)]
struct CalendarGetResponse {
    #[serde(default, rename = "labelProperties")]
    label_properties: Option<LabelProperties>,
}

#[derive(Debug, Deserialize)]
struct LabelProperties {
    #[serde(default, rename = "eventLabels")]
    event_labels: Option<Vec<GoogleEventLabel>>,
}

/// One `labelProperties.eventLabels` entry exactly as Google sends it.
#[derive(Debug, Deserialize)]
struct GoogleEventLabel {
    #[serde(default)]
    id: String,
    #[serde(default, rename = "backgroundColor")]
    background_color: String,
}

/// The cached form persisted onto `google_calendars.event_labels` — a JSON
/// array of `{"id","backgroundColor"}` (background colors canonicalized).
/// Deserializable again on the create path, where the caller's snapped hex
/// is resolved against the cache to pick the label id to send.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct CachedEventLabel {
    pub(crate) id: String,
    #[serde(rename = "backgroundColor")]
    pub(crate) background_color: String,
}

/// Fetches a calendar's `labelProperties.eventLabels` via `calendars.get` and
/// persists them as a JSON array on the `google_calendars` row.
///
/// URL: `{GOOGLE_EVENTS_BASE_URL}/{url-encoded-id}` (NOT `.../events`).
/// Absent `labelProperties.eventLabels` → `[]` (holiday/reader calendars).
/// Each `backgroundColor` is canonicalized via [`canonicalize_hex`] when it
/// parses (lowercased `#rrggbb`); the original is kept when it does not.
/// Skipped entirely when `cal.event_labels` is non-empty (already fetched);
/// Google 4xx/5xx is a hard [`CalendarError::GoogleApi`] — an import must not
/// silently leave the cache empty.
pub(crate) async fn ensure_event_labels(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    access: &GoogleAccess,
    cal: &GoogleCalendar,
    now_rfc3339: &str,
) -> Result<(), CalendarError> {
    if !cal.event_labels.is_empty() {
        return Ok(());
    }
    let url = format!(
        "{GOOGLE_EVENTS_BASE_URL}/{}",
        encode_path_segment(&cal.google_calendar_id)
    );
    let (status, body) = http.get_bearer_raw(&url, &access.access_token).await?;
    if !(200..300).contains(&status) {
        return Err(CalendarError::GoogleApi(format!(
            "calendars.get returned {status} for calendar {}",
            cal.google_calendar_id
        )));
    }
    let get: CalendarGetResponse = serde_json::from_slice(&body)
        .map_err(|err| CalendarError::InvalidResponse(format!("calendars.get body: {err}")))?;
    let labels = get
        .label_properties
        .and_then(|props| props.event_labels)
        .unwrap_or_default();
    let cached: Vec<CachedEventLabel> = labels
        .into_iter()
        .filter_map(|label| {
            if label.id.is_empty() && label.background_color.is_empty() {
                return None;
            }
            Some(CachedEventLabel {
                id: label.id,
                background_color: canonicalize_hex(&label.background_color)
                    .unwrap_or(label.background_color),
            })
        })
        .collect();
    let json = serde_json::to_string(&cached)
        .map_err(|err| CalendarError::InvalidResponse(format!("serialize event labels: {err}")))?;
    calendars
        .set_event_labels(&cal.id, &json, now_rfc3339)
        .await?;
    Ok(())
}
