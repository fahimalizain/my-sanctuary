//! `#[event(queue)]` — calendar-sync queue consumer (issue #80).
//!
//! Thin latency shell over [`api_core::run_queue_sync`]: load config, wire D1,
//! refresh the owner's token, walk the replica, notify browsers after a
//! successful publish, then map [`api_core::QueueSyncAction`] to ack / retry /
//! re-enqueue. Dirty generation plus the 15-minute fallback cron remain the
//! correctness contract — this path never throws (a throw retries the batch
//! and re-walks).

use api_core::repo::CalendarRepo;
use worker::*;

/// Queue entrypoint for the `calendar-sync` consumer binding.
///
/// worker 0.8's `#[event(queue)]` signature: `(MessageBatch<T>, Env, Context)`,
/// returning `Result<()>`. Missing config / bindings / poison payloads only
/// log and `ack_all` — never throw.
#[event(queue)]
pub async fn queue(
    batch: MessageBatch<api_core::CalendarSyncMessage>,
    env: Env,
    _ctx: Context,
) -> Result<()> {
    let Some(config) = crate::load_config(&env) else {
        console_log!("queue: config unavailable — acking batch");
        batch.ack_all();
        return Ok(());
    };
    let Some(oauth) = config.oauth.clone() else {
        console_log!("queue: oauth not configured — acking batch");
        batch.ack_all();
        return Ok(());
    };
    let Ok(db) = env.d1("DB") else {
        console_log!("queue: DB binding missing — acking batch");
        batch.ack_all();
        return Ok(());
    };

    // A fresh D1 handle per repo: `D1Database` is not Clone in worker 0.8.5,
    // but `Env::d1` returns a new wrapper around the same binding each call.
    let calendars = crate::db::D1CalendarRepo::new(db);
    let events = match env.d1("DB") {
        Ok(db) => crate::db::D1CalendarEventRepo::new(db),
        Err(err) => {
            console_log!("queue: DB binding missing: {err} — acking batch");
            batch.ack_all();
            return Ok(());
        }
    };
    let operations = match env.d1("DB") {
        Ok(db) => crate::db::D1CalendarEventOperationRepo::new(db),
        Err(err) => {
            console_log!("queue: DB binding missing: {err} — acking batch");
            batch.ack_all();
            return Ok(());
        }
    };
    let tokens = match env.d1("DB") {
        Ok(db) => crate::db::D1TokenRepo::new(db),
        Err(err) => {
            console_log!("queue: DB binding missing: {err} — acking batch");
            batch.ack_all();
            return Ok(());
        }
    };

    let messages = match batch.messages() {
        Ok(messages) => messages,
        Err(err) => {
            // Poison payload: dirty remains; cron recovers.
            console_log!("queue: batch deserialize failed: {err} — acking batch");
            batch.ack_all();
            return Ok(());
        }
    };

    /// Live wall clock for replica lease renewal (never `SystemTime` on wasm32).
    struct WorkerClock;
    impl api_core::Clock for WorkerClock {
        fn now_unix(&self) -> i64 {
            (worker::Date::now().as_millis() / 1000) as i64
        }
    }

    // max_batch_size = 1, but loop defensively.
    for message in messages {
        let body = message.body();
        let calendar_id = body.calendar_id.clone();

        let cal = match calendars.get_by_id(&calendar_id).await {
            Ok(Some(cal)) => cal,
            Ok(None) => {
                console_log!("queue: calendar {calendar_id} missing — acking");
                message.ack();
                continue;
            }
            Err(err) => {
                console_log!("queue: calendar {calendar_id} lookup failed: {err} — acking");
                message.ack();
                continue;
            }
        };

        let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
        let access = match api_core::refresh_if_needed(
            &crate::http::WorkerHttp,
            &tokens,
            &oauth,
            &cal.user_id,
            now_unix,
        )
        .await
        {
            Ok(access) => access,
            Err(err) => {
                // No tokens in the log. Do not stamp authorization_required —
                // cron owns that.
                console_log!(
                    "queue: token refresh failed for calendar {calendar_id}: {err} — acking"
                );
                message.ack();
                continue;
            }
        };

        let report = api_core::run_queue_sync(
            &crate::http::WorkerHttp,
            &calendars,
            &events,
            &operations,
            &access,
            &cal,
            now_unix,
            &WorkerClock,
        )
        .await;

        // Notify only after D1 is updated; same predicate as cron
        // (`report.published` — a replica actually published, including empty
        // incremental with a terminal token when cache_revision advanced).
        if report.published {
            crate::user_hub::notify_user(&env, &cal.user_id, Some(&cal.id)).await;
        }

        if let Some(diagnostic) = report.diagnostic {
            crate::sync_log::emit_replica_walk(diagnostic, None);
        }

        let action_label = match report.action {
            api_core::QueueSyncAction::Ack => "Ack",
            api_core::QueueSyncAction::Retry => "Retry",
            api_core::QueueSyncAction::Reenqueue => "Reenqueue",
        };
        console_log!(
            "queue: calendar={} action={} published={}",
            cal.id,
            action_label,
            report.published
        );

        match report.action {
            api_core::QueueSyncAction::Ack => {
                message.ack();
            }
            api_core::QueueSyncAction::Retry => {
                // Reserved; api-core never returns this today.
                message.retry();
            }
            api_core::QueueSyncAction::Reenqueue => {
                match env.queue("CALENDAR_SYNC") {
                    Ok(queue) => {
                        match queue
                            .send(api_core::CalendarSyncMessage {
                                calendar_id: cal.id.clone(),
                            })
                            .await
                        {
                            Ok(()) => {
                                // Follow-up is a new message; ack this one.
                                message.ack();
                            }
                            Err(err) => {
                                console_log!(
                                    "queue: reenqueue send failed for calendar {}: {err} — acking",
                                    cal.id
                                );
                                message.ack();
                            }
                        }
                    }
                    Err(err) => {
                        console_log!(
                            "queue: CALENDAR_SYNC binding missing on reenqueue for calendar {}: {err} — acking",
                            cal.id
                        );
                        message.ack();
                    }
                }
            }
        }
    }

    Ok(())
}
