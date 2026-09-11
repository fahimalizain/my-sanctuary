use worker::*;

use api_core::repo::{CalendarRepo, WatchChannelRepo};

/// `POST <WATCH_CALLBACK_URL path>` — Google Calendar push notification
/// (ADR 0001 § Webhook).
///
/// Always returns 200: Google retries any other status, so verification
/// failures (missing/unknown channel id, missing/bad token, missing or
/// disabled calendar, the `sync` handshake, missing D1 binding) are logged
/// and swallowed — never 401/404/500, and never a session or CORS check
/// (Google is not a browser).
///
/// Watches are **hints**. Contract (ADR 0005 invariant 4):
/// 1. Verify the push against the stored channel and calendar.
/// 2. Persist durable D1 work (`bump_dirty_requested` or disable) via
///    [`api_core::persist_webhook_decision`].
/// 3. Return HTTP 200.
///
/// No background replica and no token refresh on this path. Cron is the
/// recovery contract for dirty calendars and leftover channel stops.
///
/// Invoked from `fetch` (see `crate::is_webhook_request`) *before* the
/// Router so Google's POST skips session/CORS middleware — not because this
/// handler needs the fetch `Context`.
pub async fn notifications(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let headers = req.headers();
    let Some(channel_id) = headers
        .get("X-Goog-Channel-ID")?
        .filter(|id| !id.is_empty())
    else {
        console_log!("calendar webhook: missing X-Goog-Channel-ID — ignoring");
        return Response::empty();
    };
    let presented_token = headers.get("X-Goog-Channel-Token")?;
    let resource_state = headers
        .get("X-Goog-Resource-State")?
        .unwrap_or_default();

    let Ok(db) = env.d1("DB") else {
        console_log!("calendar webhook: DB binding missing — ignoring channel {channel_id}");
        return Response::empty();
    };

    let watches = crate::db::D1WatchChannelRepo::new(db);
    let channel = match watches.get_by_channel_id(&channel_id).await {
        Ok(Some(channel)) => channel,
        Ok(None) => {
            console_log!("calendar webhook: unknown channel {channel_id} — ignoring");
            return Response::empty();
        }
        Err(err) => {
            console_log!("calendar webhook: channel lookup failed: {err}");
            return Response::empty();
        }
    };

    // A fresh D1 handle per repo: `D1Database` is not Clone in worker 0.8.5,
    // but `Env::d1` returns a new wrapper around the same binding each call.
    let calendars = match env.d1("DB") {
        Ok(db) => crate::db::D1CalendarRepo::new(db),
        Err(err) => {
            console_log!("calendar webhook: DB binding missing: {err}");
            return Response::empty();
        }
    };
    // `get_by_id` filters `deleted_at IS NULL`, so `None` covers soft-deleted
    // calendars (decide_webhook rule 3).
    let calendar = match calendars.get_by_id(&channel.calendar_id).await {
        Ok(calendar) => calendar,
        Err(err) => {
            console_log!(
                "calendar webhook: calendar lookup for {} failed: {err}",
                channel.calendar_id
            );
            return Response::empty();
        }
    };

    let decision = api_core::decide_webhook(
        &resource_state,
        Some(&channel),
        presented_token.as_deref(),
        calendar.as_ref(),
    );
    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    let now_rfc3339 = api_core::unix_secs_to_rfc3339(now_unix);

    // Durable D1 write before 200. No token refresh / Google I/O on this path —
    // cron recovers dirty calendars and leftover channel stops.
    let persist = api_core::persist_webhook_decision(&calendars, &decision, &now_rfc3339).await;
    match (&decision, &persist) {
        (api_core::WebhookDecision::Ignore, api_core::WebhookPersistResult::Ignored) => {
            console_log!(
                "calendar webhook: ignored channel {channel_id} (state {resource_state:?})"
            );
        }
        (
            api_core::WebhookDecision::EnqueueDirty { calendar_id },
            api_core::WebhookPersistResult::DirtyAccepted { .. },
        ) => {
            console_log!(
                "calendar webhook: dirty accepted for calendar {calendar_id} (channel {channel_id}, state {resource_state:?})"
            );
        }
        (
            api_core::WebhookDecision::EnqueueDirty { calendar_id },
            api_core::WebhookPersistResult::DirtyPersistFailed { .. },
        ) => {
            console_log!(
                "calendar webhook: dirty write failed — not accepted for calendar {calendar_id} (channel {channel_id})"
            );
        }
        (
            api_core::WebhookDecision::CalendarGone { calendar_id },
            api_core::WebhookPersistResult::GoneDisabled { .. },
        ) => {
            console_log!(
                "calendar webhook: calendar gone — disabled {calendar_id} (channel {channel_id})"
            );
        }
        (
            api_core::WebhookDecision::CalendarGone { calendar_id },
            api_core::WebhookPersistResult::GonePersistFailed { .. },
        ) => {
            console_log!(
                "calendar webhook: disable write failed — not accepted for calendar {calendar_id} (channel {channel_id})"
            );
        }
        (decision, persist) => {
            // Decision/persist pairing should always match; log without secrets.
            console_log!(
                "calendar webhook: unexpected decision/persist pair for channel {channel_id}: decision={decision:?} persist={persist:?}"
            );
        }
    }

    Response::empty()
}
