use super::google::encode_path_segment;
use super::{CalendarError, GOOGLE_EVENTS_BASE_URL};
use crate::google_color::canonicalize_hex;
use crate::models::GoogleCalendar;
use crate::oauth::HttpClient;
use crate::repo::CalendarRepo;
use crate::time::rfc3339_to_unix_secs;
use crate::token::GoogleAccess;
use serde::{Deserialize, Serialize};

/// Event-label cache TTL. Labels rarely change; the 15-minute cron picks up
/// expiry on the next catalog/sync touch after this window.
pub(crate) const EVENT_LABELS_TTL_SECS: i64 = 24 * 60 * 60;

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

/// Whether the persisted event-label cache can be reused without a refetch.
///
/// Empty `event_labels` is never fresh. Missing / unparseable
/// `event_labels_updated_at` or unparseable `now` → not fresh. A future stamp
/// is treated as age 0 (fresh). Age is compared strictly less than
/// [`EVENT_LABELS_TTL_SECS`] (exactly 24h is stale).
pub(crate) fn event_labels_cache_is_fresh(
    event_labels: &str,
    event_labels_updated_at: Option<&str>,
    now_rfc3339: &str,
) -> bool {
    if event_labels.is_empty() {
        return false;
    }
    let Some(stamp) = event_labels_updated_at else {
        return false;
    };
    let Some(fetched_unix) = rfc3339_to_unix_secs(stamp) else {
        return false;
    };
    let Some(now_unix) = rfc3339_to_unix_secs(now_rfc3339) else {
        return false;
    };
    now_unix.saturating_sub(fetched_unix) < EVENT_LABELS_TTL_SECS
}

/// Fetches a calendar's `labelProperties.eventLabels` via `calendars.get` and
/// persists them as a JSON array on the `google_calendars` row.
///
/// URL: `{GOOGLE_EVENTS_BASE_URL}/{url-encoded-id}` (NOT `.../events`).
/// Absent `labelProperties.eventLabels` → `[]` (holiday/reader calendars).
/// Each `backgroundColor` is canonicalized via [`canonicalize_hex`] when it
/// parses (lowercased `#rrggbb`); the original is kept when it does not.
/// Skipped when the cache is non-empty and still within
/// [`EVENT_LABELS_TTL_SECS`] of `event_labels_updated_at`. Google 4xx/5xx is a
/// hard [`CalendarError::GoogleApi`] and does **not** call `set_event_labels`
/// (previous payload and stamp stay put so the next catalog/sync retries).
pub(crate) async fn ensure_event_labels(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    access: &GoogleAccess,
    cal: &GoogleCalendar,
    now_rfc3339: &str,
) -> Result<(), CalendarError> {
    if event_labels_cache_is_fresh(
        &cal.event_labels,
        cal.event_labels_updated_at.as_deref(),
        now_rfc3339,
    ) {
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
