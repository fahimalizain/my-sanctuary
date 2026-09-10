//! Outbound insert journal: client-supplied Google event ids, payload
//! fingerprints, and the journaled `events.insert` path (issue #50 / V4).
//!
//! Create flow: mint id → stamp shared props → fingerprint → journal
//! `pending` → Google POST → `google_committed` (or 409+GET) → cache upsert
//! → `cache_applied`. Cache failure after Google commit is a hard
//! [`CalendarError::Repo`] (never a 200 with a phantom local id).
//!
//! Patch/delete live in [`super::write_journal`].

use sha2::{Digest, Sha256};

use super::apply::{map_google_event, row_from_new_event, GoogleEvent};
use super::google::encode_path_segment;
use super::labels::CachedEventLabel;
use super::write::{build_shared_properties, CreateEventOutput};
use super::{CalendarError, GOOGLE_EVENTS_BASE_URL};
use crate::google_color::snap_to_event_label_hex;
use crate::models::{
    NewCalendarEventOperation, NewEventInput, OP_STATUS_CACHE_APPLIED, OP_STATUS_FAILED,
    OP_STATUS_GOOGLE_COMMITTED, OP_STATUS_PENDING, OP_VERB_INSERT,
};
use crate::oauth::HttpClient;
use crate::repo::{CalendarEventOperationRepo, CalendarEventRepo, CalendarRepo};
use crate::time::unix_secs_to_rfc3339;
use crate::token::GoogleAccess;


/// Google client-supplied event id: `sanc` + 32 lowercase hex chars.
///
/// Hex is a subset of Google's base32hex alphabet `[0-9a-v]`; length 36 is
/// within the 5–1024 bound. Minted once per create and reused on 409 retry
/// (GET by id — never a second POST with a new id).
pub(crate) fn mint_google_event_id() -> String {
    let mut bytes = [0u8; 16];
    let _ = getrandom::getrandom(&mut bytes);
    let mut id = String::with_capacity(4 + 32);
    id.push_str("sanc");
    id.push_str(&hex_encode(&bytes));
    id
}

/// SHA-256 hex (64 lowercase chars) of the exact POST body bytes.
pub(crate) fn payload_fingerprint(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex_encode(&digest)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

/// Journaled `events.insert`: persist the outbound op before Google, reuse a
/// client-supplied event id, treat 409 as success via GET, and return the
/// V2 persisted local id (never a phantom).
pub(crate) async fn create_event_with_journal(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    operations: &dyn CalendarEventOperationRepo,
    access: &GoogleAccess,
    input: &NewEventInput,
    now_unix: i64,
) -> Result<CreateEventOutput, CalendarError> {
    let Some(cal) = calendars.get_by_id(&input.calendar_id).await? else {
        return Err(CalendarError::NotFound);
    };

    let minted_id = mint_google_event_id();
    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);

    let mut payload = serde_json::json!({
        "id": minted_id,
        "summary": input.summary,
        "description": input.description,
        "start": { "dateTime": input.start },
        "end": { "dateTime": input.end },
    });
    let shared = build_shared_properties(input, &minted_id);
    payload["extendedProperties"] = serde_json::json!({ "shared": shared });

    // Category color → Google event label: snap the caller's hex onto the 24
    // event-label palette, then resolve the label's id against the calendar's
    // CACHED `event_labels`. A cache miss or missing match fails before any
    // journal row or Google call. `colorId` is never sent.
    let url = match input.color_hex.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(color_hex) => {
            let snapped = snap_to_event_label_hex(color_hex)
                .map_err(|err| CalendarError::Invalid(err.to_string()))?;
            if cal.event_labels.is_empty() {
                return Err(CalendarError::Invalid(
                    "calendar event-label cache is empty".to_string(),
                ));
            }
            let labels: Vec<CachedEventLabel> = serde_json::from_str(&cal.event_labels)
                .map_err(|err| {
                    CalendarError::Invalid(format!("invalid calendar event-label cache: {err}"))
                })?;
            let label_id = labels
                .iter()
                .find(|label| label.background_color.eq_ignore_ascii_case(&snapped))
                .map(|label| label.id.clone())
                .ok_or_else(|| {
                    CalendarError::Invalid("no event label matches category color".to_string())
                })?;
            payload["eventLabelId"] = serde_json::json!(label_id);
            format!(
                "{GOOGLE_EVENTS_BASE_URL}/{}/events?eventLabelVersion=1",
                encode_path_segment(&cal.google_calendar_id)
            )
        }
        None => format!(
            "{GOOGLE_EVENTS_BASE_URL}/{}/events",
            encode_path_segment(&cal.google_calendar_id)
        ),
    };

    let body =
        serde_json::to_vec(&payload).map_err(|err| CalendarError::InvalidResponse(err.to_string()))?;
    let fingerprint = payload_fingerprint(&body);
    let payload_json = String::from_utf8(body.clone()).map_err(|err| {
        CalendarError::InvalidResponse(format!("events.insert payload utf-8: {err}"))
    })?;

    let op_id = operations
        .insert(
            NewCalendarEventOperation {
                user_id: cal.user_id.clone(),
                calendar_id: cal.id.clone(),
                local_event_id: String::new(),
                google_event_id: minted_id.clone(),
                verb: OP_VERB_INSERT.to_string(),
                payload_fingerprint: fingerprint,
                payload_json,
                status: OP_STATUS_PENDING.to_string(),
                google_etag: String::new(),
            },
            &now_rfc3339,
        )
        .await?;

    let (status, response) = http.post_json(&url, &access.access_token, &body).await?;

    let created = match status {
        s if (200..300).contains(&s) => parse_google_event(&response, "events.insert")?,
        409 => {
            // Duplicate client id: the prior insert landed. GET by minted id
            // — never mint a second id, never POST again.
            let get_url = format!(
                "{GOOGLE_EVENTS_BASE_URL}/{}/events/{}",
                encode_path_segment(&cal.google_calendar_id),
                encode_path_segment(&minted_id)
            );
            let (get_status, get_body) = http
                .get_bearer_raw(&get_url, &access.access_token)
                .await?;
            if !(200..300).contains(&get_status) {
                let msg = format!(
                    "google events.insert returned 409; GET returned {get_status}"
                );
                let _ = operations
                    .update_status(&op_id, OP_STATUS_FAILED, &msg, &now_rfc3339)
                    .await;
                return Err(CalendarError::GoogleApi(msg));
            }
            parse_google_event(&get_body, "events.get after 409")?
        }
        other => {
            let msg = format!("google events.insert returned {other}");
            let _ = operations
                .update_status(&op_id, OP_STATUS_FAILED, &msg, &now_rfc3339)
                .await;
            return Err(CalendarError::GoogleApi(msg));
        }
    };

    let google_id = if created.id.is_empty() {
        minted_id.clone()
    } else {
        created.id.clone()
    };
    let etag = created.etag.clone().unwrap_or_default();

    operations
        .update_progress(
            &op_id,
            OP_STATUS_GOOGLE_COMMITTED,
            &google_id,
            "",
            &etag,
            "",
            false,
            &now_rfc3339,
        )
        .await?;

    let new_event = map_google_event(&created, &cal.id, &now_rfc3339);
    let local_id = match events.upsert(new_event.clone(), &now_rfc3339).await {
        Ok(id) => id,
        Err(err) => {
            // Leave status google_committed — repair (slice 4) will finish.
            return Err(CalendarError::Repo(err));
        }
    };

    operations
        .update_progress(
            &op_id,
            OP_STATUS_CACHE_APPLIED,
            &google_id,
            &local_id,
            &etag,
            "",
            false,
            &now_rfc3339,
        )
        .await?;

    // Best-effort dirty bump — do not fail a successful create.
    let _ = calendars.bump_dirty_requested(&cal.id, &now_rfc3339).await;

    Ok(CreateEventOutput {
        event: row_from_new_event(new_event, local_id, &now_rfc3339),
        source: "google".to_string(),
        cache_error: None,
    })
}

pub(crate) fn parse_google_event(bytes: &[u8], context: &str) -> Result<GoogleEvent, CalendarError> {
    serde_json::from_slice(bytes)
        .map_err(|err| CalendarError::InvalidResponse(format!("{context} body: {err}")))
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_id_is_sanc_plus_32_base32hex_chars() {
        let id = mint_google_event_id();
        assert_eq!(id.len(), 36, "sanc + 32 hex: {id}");
        assert!(id.starts_with("sanc"), "{id}");
        assert!(
            id.chars().all(|c| matches!(c, '0'..='9' | 'a'..='v')),
            "only base32hex [0-9a-v]: {id}"
        );
    }

    #[test]
    fn fingerprint_is_deterministic_full_sha256_hex() {
        let a = payload_fingerprint(b"{\"summary\":\"hi\"}");
        let b = payload_fingerprint(b"{\"summary\":\"hi\"}");
        let c = payload_fingerprint(b"{\"summary\":\"bye\"}");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 64);
        assert!(
            a.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
            "lowercase hex: {a}"
        );
        let digest = Sha256::digest(b"{\"summary\":\"hi\"}");
        assert_eq!(a, hex_encode(&digest));
    }
}
