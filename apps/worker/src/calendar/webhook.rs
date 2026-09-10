use worker::*;

use api_core::repo::{CalendarRepo, WatchChannelRepo};

/// `POST <WATCH_CALLBACK_URL path>` — Google Calendar push notification
/// (ADR 0001 § Webhook).
///
/// Always returns 200: Google retries any other status, so verification
/// failures (missing/unknown channel id, missing/bad token, missing or
/// disabled calendar, the `sync` handshake, missing config or D1 binding)
/// are logged and swallowed — never 401/404/500, and never a session or CORS
/// check (Google is not a browser).
///
/// Watches are **hints**. A verified `exists` is accepted work only after a
/// durable D1 dirty write (`persist_webhook_decision`); `not_exists` disables
/// the calendar the same way. HTTP 200 is returned **after** that persist
/// (or after deciding Ignore). An optional `ctx.wait_until` replica attempt
/// (token refresh + `sync_calendar` / `stop_watches_for_calendar`) is only an
/// optimization after durable work lands — a dead isolate cannot drop the
/// dirty generation; cron recovers lost background work.
///
/// Invoked from `fetch` (see `crate::is_webhook_request`) *before* the
/// Router, because `Router::run` never sees the fetch `Context` that
/// `wait_until` needs.
pub async fn notifications(req: Request, env: Env, ctx: Context) -> Result<Response> {
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

    let Some(config) = crate::load_config(&env) else {
        console_log!("calendar webhook: config unavailable — ignoring channel {channel_id}");
        return Response::empty();
    };
    let Some(oauth) = config.oauth.as_ref().cloned() else {
        console_log!("calendar webhook: oauth not configured — ignoring channel {channel_id}");
        return Response::empty();
    };
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

    // Durable D1 write before 200. Token refresh / Google I/O must not block
    // this path — they run only inside optional wait_until after accept.
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
            if let Some(calendar) = calendar {
                // Optional replica attempt: refresh + sync inside wait_until so
                // they never block the 200. Dirty is already durable.
                ctx.wait_until(async move {
                    let tokens = match env.d1("DB") {
                        Ok(db) => crate::db::D1TokenRepo::new(db),
                        Err(err) => {
                            console_log!(
                                "calendar webhook: background sync for {} skipped (DB binding missing): {err}",
                                calendar.id
                            );
                            return;
                        }
                    };
                    let access = match api_core::refresh_if_needed(
                        &crate::http::WorkerHttp,
                        &tokens,
                        &oauth,
                        &calendar.user_id,
                        now_unix,
                    )
                    .await
                    {
                        Ok(access) => access,
                        Err(err) => {
                            console_log!(
                                "calendar webhook: token refresh for user {} failed (dirty already durable): {err}",
                                calendar.user_id
                            );
                            return;
                        }
                    };
                    let calendars = match env.d1("DB") {
                        Ok(db) => crate::db::D1CalendarRepo::new(db),
                        Err(err) => {
                            console_log!(
                                "calendar webhook: background sync for {} skipped (DB binding missing): {err}",
                                calendar.id
                            );
                            return;
                        }
                    };
                    let events = match env.d1("DB") {
                        Ok(db) => crate::db::D1CalendarEventRepo::new(db),
                        Err(err) => {
                            console_log!(
                                "calendar webhook: background sync for {} skipped (DB binding missing): {err}",
                                calendar.id
                            );
                            return;
                        }
                    };
                    let operations = match env.d1("DB") {
                        Ok(db) => crate::db::D1CalendarEventOperationRepo::new(db),
                        Err(err) => {
                            console_log!(
                                "calendar webhook: background sync for {} skipped (DB binding missing): {err}",
                                calendar.id
                            );
                            return;
                        }
                    };
                    match api_core::sync_calendar(
                        &crate::http::WorkerHttp,
                        &calendars,
                        &events,
                        &operations,
                        &access,
                        &calendar,
                        &now_rfc3339,
                    )
                    .await
                    {
                        Ok(api_core::SyncCalendarOutcome::Published) => {
                            crate::user_hub::notify_user(
                                &env,
                                &calendar.user_id,
                                Some(&calendar.id),
                            )
                            .await;
                        }
                        Ok(api_core::SyncCalendarOutcome::LeaseBusy) => {
                            console_log!(
                                "calendar webhook: background sync for {} skipped (lease busy)",
                                calendar.id
                            );
                        }
                        Err(err) => {
                            console_log!(
                                "calendar webhook: background sync for {} failed: {err}",
                                calendar.id
                            );
                        }
                    }
                });
            }
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
            if let Some(calendar) = calendar {
                // Optional: stop leftover channels after disable. Failures leave
                // channel rows for a later retry (slice 4 / cron).
                ctx.wait_until(async move {
                    let tokens = match env.d1("DB") {
                        Ok(db) => crate::db::D1TokenRepo::new(db),
                        Err(err) => {
                            console_log!(
                                "calendar webhook: stop watches for {} skipped (DB binding missing): {err}",
                                calendar.id
                            );
                            return;
                        }
                    };
                    let access = match api_core::refresh_if_needed(
                        &crate::http::WorkerHttp,
                        &tokens,
                        &oauth,
                        &calendar.user_id,
                        now_unix,
                    )
                    .await
                    {
                        Ok(access) => access,
                        Err(err) => {
                            console_log!(
                                "calendar webhook: token refresh for user {} failed (disable already durable): {err}",
                                calendar.user_id
                            );
                            return;
                        }
                    };
                    let watches = match env.d1("DB") {
                        Ok(db) => crate::db::D1WatchChannelRepo::new(db),
                        Err(err) => {
                            console_log!(
                                "calendar webhook: stop watches for {} skipped (DB binding missing): {err}",
                                calendar.id
                            );
                            return;
                        }
                    };
                    if let Err(err) = api_core::stop_watches_for_calendar(
                        &crate::http::WorkerHttp,
                        &watches,
                        &access,
                        &calendar.id,
                    )
                    .await
                    {
                        console_log!(
                            "calendar webhook: stop watches for {} failed (channel rows kept): {err}",
                            calendar.id
                        );
                        return;
                    }
                    // Stamp sanitized coverage after stop (→ missing). Best-effort.
                    let calendars = match env.d1("DB") {
                        Ok(db) => crate::db::D1CalendarRepo::new(db),
                        Err(err) => {
                            console_log!(
                                "calendar webhook: watch coverage refresh for {} skipped (DB binding missing): {err}",
                                calendar.id
                            );
                            return;
                        }
                    };
                    if let Err(err) = api_core::refresh_watch_coverage(
                        &calendars,
                        &watches,
                        &calendar.id,
                        now_unix,
                        &now_rfc3339,
                    )
                    .await
                    {
                        console_log!(
                            "calendar webhook: watch coverage refresh for {} failed: {err}",
                            calendar.id
                        );
                    }
                });
            }
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
