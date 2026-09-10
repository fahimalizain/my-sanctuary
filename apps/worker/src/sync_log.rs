//! Structured operator log lines for replica walks and sync warnings.
//!
//! Prefixes `replica_walk` and `sync_warning` are stable so operators can grep
//! Cloudflare logs. Payloads are serde snake_case of the already-sanitized
//! api-core types — never cursors, tokens, credentials, or event bodies.

use api_core::{OperatorWarningRecord, ReplicaWalkDiagnostic};
use worker::console_log;

/// Stamp `APP_VERSION` when empty, then `console_log!("replica_walk {json}")`.
///
/// If `duration_ms` is `Some`, write it onto the record first (webhook path
/// measures with `worker::Date` ms). Serialize failure logs
/// `replica_walk: serialize failed` without the record.
pub fn emit_replica_walk(mut diagnostic: ReplicaWalkDiagnostic, duration_ms: Option<i64>) {
    if let Some(ms) = duration_ms {
        diagnostic.duration_ms = ms;
    }
    if diagnostic.deployed_version.is_empty() {
        diagnostic.deployed_version = env!("APP_VERSION").to_string();
    }
    match serde_json::to_string(&diagnostic) {
        Ok(json) => console_log!("replica_walk {json}"),
        Err(_) => console_log!("replica_walk: serialize failed"),
    }
}

/// `console_log!("sync_warning {json}")` for one operator warning.
pub fn emit_operator_warning(warning: &OperatorWarningRecord) {
    match serde_json::to_string(warning) {
        Ok(json) => console_log!("sync_warning {json}"),
        Err(_) => console_log!("sync_warning: serialize failed"),
    }
}
