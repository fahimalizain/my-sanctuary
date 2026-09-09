use super::google::encode_path_segment;
use super::{
    CalendarError, GOOGLE_CHANNELS_STOP_URL, GOOGLE_EVENTS_BASE_URL, WATCH_DEFAULT_TTL_SECS,
    WATCH_RENEW_HORIZON_SECS,
};
use crate::models::{GoogleCalendar, NewWatchChannel, WatchChannel};
use crate::oauth::HttpClient;
use crate::repo::WatchChannelRepo;
use crate::time::unix_secs_to_rfc3339;
use crate::token::GoogleAccess;
use serde::de::{self, Deserializer, Visitor};
use serde::Deserialize;
use std::fmt;
use url::Url;

// ──────────────────────────────────────────
// Watch channels (ADR 0001)
// ──────────────────────────────────────────

/// Whether `url` is a callback Google may push webhooks to: it parses as a
/// URL, has scheme `https`, and its host is not a loopback address
/// (`localhost`, `127.0.0.1`, `::1` — host comparison is case-insensitive).
/// Empty strings, unparseable values, and missing hosts are `false`.
///
/// Google refuses to deliver push notifications to non-public addresses, and
/// watching from local `wrangler dev` would leak a channel we cannot consume —
/// so `list_events` treats a non-public callback as "skip all watch I/O".
pub fn is_public_https_callback(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    if parsed.scheme() != "https" {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    // `url::Url` renders IPv6 hosts with brackets (`[::1]`); strip them so the
    // loopback comparison sees the bare address.
    let host = host
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host)
        .to_ascii_lowercase();
    host != "localhost" && host != "127.0.0.1" && host != "::1"
}

/// Mints the watch `channel_id`: 16 random bytes formatted as a UUID string
/// (`8-4-4-4-12` hex, 36 chars — well under Google's 64-char limit). The UUID
/// shape is cosmetic; it is the `X-Goog-Channel-ID` webhook lookup key.
fn mint_channel_id() -> String {
    let mut bytes = [0u8; 16];
    // Same randomness source as oauth::generate_state (OS entropy natively,
    // Web Crypto on wasm); failure is practically impossible.
    let _ = getrandom::getrandom(&mut bytes);
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-\
         {:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8],
        bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}

/// Mints the webhook `token`: 32 random bytes hex-encoded (64 hex chars).
/// Compared against `X-Goog-Channel-Token` by the webhook handler. Never
/// contains OAuth tokens or other secrets.
fn mint_token() -> String {
    let mut bytes = [0u8; 32];
    let _ = getrandom::getrandom(&mut bytes);
    let mut out = String::with_capacity(64);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Google's `events.watch` success body (subset): the channel `id` we minted
/// (ignored — we store ours), Google's `resourceId`, and the channel
/// `expiration` in Unix **milliseconds**.
#[derive(Debug, Deserialize)]
pub(crate) struct WatchChannelResponse {
    #[serde(rename = "resourceId")]
    pub(crate) resource_id: String,
    #[serde(default, rename = "expiration", deserialize_with = "de_optional_expiration")]
    pub(crate) expiration_millis: Option<i64>,
}

/// Deserializes `Channel.expiration` (Unix ms) into `Option<i64>`. Google's
/// discovery doc types it `string`/`int64`, so `events.watch` sends it as a
/// JSON string of digits (`"1787628641000"`), while some responses send a
/// JSON number. `null`/missing → `None` (callers fall back to the default 7-day
/// TTL); an unparseable string is an error, never a silent `None`.
fn de_optional_expiration<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    struct ExpirationVisitor;

    impl<'de> Visitor<'de> for ExpirationVisitor {
        type Value = Option<i64>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("an integer, a string of digits, or null")
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
            Ok(Some(value))
        }
        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            i64::try_from(value)
                .map(Some)
                .map_err(|_| de::Error::invalid_value(de::Unexpected::Unsigned(value), &self))
        }
        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            value
                .parse::<i64>()
                .map(Some)
                .map_err(|_| de::Error::invalid_value(de::Unexpected::Str(value), &self))
        }
        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            self.visit_str(&value)
        }
    }

    deserializer.deserialize_any(ExpirationVisitor)
}

/// POSTs `events.watch` for `cal` (with a freshly minted `channel_id`/`token`)
/// and inserts the returned [`NewWatchChannel`] from Google's `resourceId` and
/// the converted expiration (Unix ms → RFC 3339 UTC; 7 days from `now_unix`
/// when Google omits it).
///
/// Shared by [`ensure_watch`] and [`renew_watch_if_needed`]. Watch HTTP 404 →
/// [`CalendarError::GoogleNotFound`] so the caller can disable sync (same as
/// an `events.list` 404). Other non-2xx → `GoogleApi`.
async fn create_watch(
    http: &dyn HttpClient,
    watches: &dyn WatchChannelRepo,
    access: &GoogleAccess,
    cal: &GoogleCalendar,
    callback_url: &str,
    now_unix: i64,
) -> Result<(), CalendarError> {
    let channel_id = mint_channel_id();
    let token = mint_token();
    let url = format!(
        "{GOOGLE_EVENTS_BASE_URL}/{}/events/watch",
        encode_path_segment(&cal.google_calendar_id)
    );
    let payload = serde_json::json!({
        "id": channel_id,
        "type": "web_hook",
        "address": callback_url,
        "token": token,
    });
    let body =
        serde_json::to_vec(&payload).map_err(|err| CalendarError::InvalidResponse(err.to_string()))?;
    let (status, response) = http.post_json(&url, &access.access_token, &body).await?;
    if status == 404 {
        return Err(CalendarError::GoogleNotFound);
    }
    if !(200..300).contains(&status) {
        return Err(CalendarError::GoogleApi(format!(
            "google events.watch returned {status}"
        )));
    }
    let channel: WatchChannelResponse = serde_json::from_slice(&response)
        .map_err(|err| CalendarError::InvalidResponse(format!("events.watch body: {err}")))?;
    let expiration_secs = channel
        .expiration_millis
        .map(|millis| millis / 1000)
        .unwrap_or(now_unix + WATCH_DEFAULT_TTL_SECS);
    watches
        .insert(
            NewWatchChannel {
                calendar_id: cal.id.clone(),
                channel_id,
                resource_id: channel.resource_id,
                token,
                expiration: unix_secs_to_rfc3339(expiration_secs),
            },
            &unix_secs_to_rfc3339(now_unix),
        )
        .await?;
    Ok(())
}

/// Ensures a Google `events.watch` channel exists for `cal`.
///
/// Returns `Ok` when an unexpired channel row is already stored for the
/// calendar; otherwise POSTs `events.watch` via [`create_watch`] and inserts
/// the channel row. Note that "unexpired" only means `expiration > now` — a
/// channel with 23 hours left still short-circuits here; renewal is the
/// fallback cron's job ([`renew_watch_if_needed`]).
pub async fn ensure_watch(
    http: &dyn HttpClient,
    watches: &dyn WatchChannelRepo,
    access: &GoogleAccess,
    cal: &GoogleCalendar,
    callback_url: &str,
    now_unix: i64,
) -> Result<(), CalendarError> {
    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);
    if !watches
        .list_unexpired_by_calendar_id(&cal.id, &now_rfc3339)
        .await?
        .is_empty()
    {
        return Ok(());
    }
    create_watch(http, watches, access, cal, callback_url, now_unix).await
}

/// POSTs `channels.stop` for one channel; HTTP 404 counts as success (the
/// channel is already gone). Any other non-2xx is an error.
async fn stop_channel(
    http: &dyn HttpClient,
    access: &GoogleAccess,
    channel: &WatchChannel,
) -> Result<(), CalendarError> {
    let payload = serde_json::json!({
        "id": channel.channel_id,
        "resourceId": channel.resource_id,
    });
    let body =
        serde_json::to_vec(&payload).map_err(|err| CalendarError::InvalidResponse(err.to_string()))?;
    let (status, _response) =
        http.post_json(GOOGLE_CHANNELS_STOP_URL, &access.access_token, &body).await?;
    if status == 404 {
        return Ok(()); // already gone — success
    }
    if !(200..300).contains(&status) {
        return Err(CalendarError::GoogleApi(format!(
            "google channels.stop returned {status}"
        )));
    }
    Ok(())
}

/// Stops every stored watch channel for `calendar_id` via `channels.stop`
/// (HTTP 404 counts as success — the channel is already gone) and then HARD
/// deletes the rows (ADR 0001).
///
/// Channels are stopped sequentially. On the first hard failure this returns
/// the error **before** deleting anything: rows that were not stopped keep
/// their `{id, resourceId}` so a later run can retry them, and earlier rows
/// that stopped successfully may already be dead on Google's side but their
/// rows are only removed once every stop succeeds.
pub async fn stop_watches_for_calendar(
    http: &dyn HttpClient,
    watches: &dyn WatchChannelRepo,
    access: &GoogleAccess,
    calendar_id: &str,
) -> Result<(), CalendarError> {
    let channels = watches.list_by_calendar_id(calendar_id).await?;
    for channel in &channels {
        stop_channel(http, access, channel).await?;
    }
    watches.delete_by_calendar_id(calendar_id).await?;
    Ok(())
}

/// Renews a calendar's watch channel when none covers `WATCH_RENEW_HORIZON_SECS`
/// from `now_unix` (ADR 0001 § Fallback cron).
///
/// Horizon is 24h (`WATCH_RENEW_HORIZON_SECS`); cron already renews before
/// coverage goes thin — no separate health-column alert for under-coverage.
///
/// `ensure_watch` only checks that some channel is unexpired (`expiration >
/// now`) — a channel with 23 hours left would skip it. Renewal instead mints
/// a new channel whenever no stored channel expires later than `now_unix +
/// WATCH_RENEW_HORIZON_SECS`, then stops and hard-deletes the **old** rows
/// individually — never `delete_by_calendar_id`, which would kill the new row.
///
/// Returns `Ok(true)` when a new channel was created, `Ok(false)` when the
/// existing coverage already spans the horizon. Watch HTTP 404 →
/// [`CalendarError::GoogleNotFound`] (the caller disables sync and stops any
/// prior channels). If stopping an old channel fails, the error is returned
/// and the new channel row remains — overlap of two rows per calendar is
/// expected (ADR 0001).
pub async fn renew_watch_if_needed(
    http: &dyn HttpClient,
    watches: &dyn WatchChannelRepo,
    access: &GoogleAccess,
    cal: &GoogleCalendar,
    callback_url: &str,
    now_unix: i64,
) -> Result<bool, CalendarError> {
    let existing = watches.list_by_calendar_id(&cal.id).await?;
    let horizon = unix_secs_to_rfc3339(now_unix + WATCH_RENEW_HORIZON_SECS);
    // RFC 3339 UTC strings of this shape compare lexicographically.
    if existing.iter().any(|channel| channel.expiration > horizon) {
        return Ok(false);
    }

    create_watch(http, watches, access, cal, callback_url, now_unix).await?;
    for old in &existing {
        stop_channel(http, access, old).await?;
        watches.delete_by_id(&old.id).await?;
    }
    Ok(true)
}
