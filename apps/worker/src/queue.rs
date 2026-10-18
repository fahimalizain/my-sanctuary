//! `#[event(queue)]` — calendar-sync queue consumer (issue #80).
//!
//! Thin latency shell over [`api_core::run_queue_sync`]: load config, wire D1,
//! refresh the owner's token, walk the replica, notify browsers after a
//! successful publish, then map [`api_core::QueueSyncAction`] to ack / retry /
//! re-enqueue. Dirty generation plus the 15-minute fallback cron remain the
//! correctness contract — this path never throws (a throw retries the batch
//! and re-walks).
//!
//! Since reports slice 2 the same binding carries a second payload shape:
//! [`api_core::DigestQueueMessage`] asks for a digest rebuild/reclassify. The
//! batch is typed as [`api_core::WorkerQueueMessage`], an untagged enum whose
//! `CalendarSync` variant comes first, so `{"calendar_id":"…"}` still
//! deserializes exactly as it always did.

use api_core::repo::CalendarRepo;
use worker::*;

/// One digest tick for one user, logging the report. Never throws: a digest
/// failure is a logged report error, and the cron picks the work up again.
async fn run_digest_for_user(env: &Env, user_id: &str, now_unix: i64) -> Result<()> {
    let repo = match env.d1("DB") {
        Ok(db) => crate::db::D1DigestRepo::new(db),
        Err(err) => {
            console_log!("queue: DB binding missing for digest: {err}");
            return Ok(());
        }
    };
    let categories = match env.d1("DB") {
        Ok(db) => crate::db::D1TaskCategoryRepo::new(db),
        Err(err) => {
            console_log!("queue: DB binding missing for digest: {err}");
            return Ok(());
        }
    };
    let report = api_core::run_digest_tick_user(
        &repo,
        &categories,
        user_id,
        api_core::DIGEST_TICK_BUDGET_DAYS,
        now_unix,
    )
    .await;
    for error in &report.errors {
        console_log!("queue: digest: {error}");
    }
    console_log!(
        "queue: digest user={} rebuilt={} reclassified={} remaining={}",
        user_id,
        report.rebuilt_days,
        report.reclassified,
        report.remaining
    );
    if report.remaining {
        // Same pattern as the calendar re-enqueue: the follow-up is a new
        // message, so this one is acked either way.
        match env.queue("CALENDAR_SYNC") {
            Ok(queue) => {
                let message = api_core::DigestQueueMessage {
                    kind: api_core::DigestJobKind::Rebuild,
                    user_id: user_id.to_string(),
                };
                if let Err(err) = queue.send(message).await {
                    console_log!("queue: digest re-enqueue failed for {user_id}: {err}");
                }
            }
            Err(err) => {
                console_log!("queue: CALENDAR_SYNC binding missing on digest re-enqueue: {err}");
            }
        }
    }
    Ok(())
}

/// Queue entrypoint for the `calendar-sync` consumer binding.
///
/// worker 0.8's `#[event(queue)]` signature: `(MessageBatch<T>, Env, Context)`,
/// returning `Result<()>`. Missing config / bindings / poison payloads only
/// log and `ack_all` — never throw.
#[event(queue)]
pub async fn queue(
    batch: MessageBatch<api_core::WorkerQueueMessage>,
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
    let categories = match env.d1("DB") {
        Ok(db) => crate::db::D1TaskCategoryRepo::new(db),
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
        // Digest jobs are a separate, self-contained path: the tick owns the
        // retry contract (cron re-sees a `running` job), so failures are acked
        // rather than re-delivered. The `CalendarSync` arm keeps today's flow.
        let calendar_id = match message.body() {
            api_core::WorkerQueueMessage::Digest(digest) => {
                let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
                run_digest_for_user(&env, &digest.user_id, now_unix).await?;
                message.ack();
                continue;
            }
            api_core::WorkerQueueMessage::CalendarSync(sync) => sync.calendar_id.clone(),
        };

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
            &categories,
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
