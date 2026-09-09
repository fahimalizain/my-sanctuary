//! `/api/calendar/*` handlers: cached event listing (with Google sync), event
//! creation, and the Google push-notification webhook, mirroring the old Go
//! `handlers/calendar.go`.
//!
//! The list/create endpoints are session-gated and refresh the Google access
//! token when stale — a user whose token cannot be refreshed gets `401
//! {"error":"unauthorized"}`, exactly like the Go handlers. The webhook is
//! unauthenticated by design (ADR 0001 § Webhook): Google cannot send a
//! session cookie, so verification is the `X-Goog-Channel-*` headers, and
//! every failure is swallowed into a 200.
//!
//! The sync/create/webhook orchestration lives in `api_core::calendar` (pure,
//! unit-tested); this module only extracts the session user (or the webhook
//! headers), wires the D1 repos and `WorkerHttp`, and maps errors to HTTP
//! responses.
//!
//! - [`http`] — REST: list/create/patch/delete events and list calendars
//! - [`webhook`] — Google push-notification handler (`notifications`)

mod http;
mod webhook;

pub use http::{create_event, delete_event, list_calendars, list_events, update_event};
pub use webhook::notifications;
