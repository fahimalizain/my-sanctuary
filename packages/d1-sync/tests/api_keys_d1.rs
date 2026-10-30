//! API keys against the real Worker migrations (local SQLite = D1 schema).
//!
//! Applies `apps/worker/migrations/*.sql`, seeds user `u-1`, then drives
//! [`SqliteApiKeyRepo`] through insert / list / authenticate / revoke /
//! last-used. Every timestamp is a fixed string — no test reads the clock.

use std::sync::Arc;

use api_core::api_key::{ApiKey, ApiKeyRepo, NewApiKey};
use d1_sync::{open_harness, query_one, seed_user_token_calendar, SeedOpts, SqliteApiKeyRepo};
use uuid::Uuid;

/// First of two creation stamps; the list must put the newer key first.
const CREATED_EARLY: &str = "2026-10-01T08:00:00Z";
const CREATED_LATE: &str = "2026-10-02T08:00:00Z";
/// The frozen "request now" every test authenticates at.
const NOW: &str = "2026-10-10T12:00:00Z";
/// The throttle cutoff paired with `NOW` (one hour earlier).
const CUTOFF: &str = "2026-10-10T11:00:00Z";
/// A far-future expiry — effectively "never" without using NULL.
const FAR_FUTURE: &str = "2099-01-01T00:00:00Z";

fn repo(h: &d1_sync::Harness) -> SqliteApiKeyRepo {
    SqliteApiKeyRepo::new(Arc::clone(&h.db))
}

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

/// A live key for `u-1` with a far-future expiry.
fn key(id: &str, name: &str, key_hash: &str, created_at: &str) -> NewApiKey {
    NewApiKey {
        id: id.to_string(),
        user_id: "u-1".to_string(),
        name: name.to_string(),
        key_hash: key_hash.to_string(),
        key_prefix: "sk_abcd1234".to_string(),
        expires_on: Some("2099-01-01".to_string()),
        expires_at: Some(FAR_FUTURE.to_string()),
        created_at: created_at.to_string(),
        updated_at: created_at.to_string(),
    }
}

/// Opens a harness and seeds user `u-1` (the FK target for every key).
fn harness() -> d1_sync::Harness {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().expect("db");
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed user");
    }
    h
}

#[test]
fn insert_then_list_returns_living_rows_newest_first() {
    let h = harness();
    let repo = repo(&h);

    let older = key(&new_id(), "Old laptop", "hash-old", CREATED_EARLY);
    let newer = key(&new_id(), "New laptop", "hash-new", CREATED_LATE);
    let inserted_older = pollster::block_on(repo.insert(older.clone())).expect("insert older");
    let inserted_newer = pollster::block_on(repo.insert(newer.clone())).expect("insert newer");

    // The returned row is the input plus the two unset columns.
    assert_eq!(inserted_older.id, older.id);
    assert_eq!(inserted_older.user_id, "u-1");
    assert_eq!(inserted_older.name, "Old laptop");
    assert_eq!(inserted_older.key_hash, "hash-old");
    assert_eq!(inserted_older.key_prefix, "sk_abcd1234");
    assert_eq!(inserted_older.expires_on.as_deref(), Some("2099-01-01"));
    assert_eq!(inserted_older.expires_at.as_deref(), Some(FAR_FUTURE));
    assert_eq!(inserted_older.last_used_at, None);
    assert_eq!(inserted_older.deleted_at, None);
    assert_eq!(inserted_older.created_at, CREATED_EARLY);
    assert_eq!(inserted_older.updated_at, CREATED_EARLY);

    // Reading back must round-trip every column in created_at DESC order.
    let listed = pollster::block_on(repo.list_by_user_id("u-1")).expect("list");
    assert_eq!(listed, vec![inserted_newer, inserted_older]);

    // Scoped to the owner: a user with no keys sees none.
    assert!(pollster::block_on(repo.list_by_user_id("u-2"))
        .expect("list other")
        .is_empty());
}

#[test]
fn authenticate_returns_the_owner_for_a_live_key() {
    let h = harness();
    let repo = repo(&h);
    let id = new_id();
    pollster::block_on(repo.insert(key(&id, "Laptop", "hash-live", CREATED_EARLY)))
        .expect("insert");

    let owner = pollster::block_on(repo.authenticate("hash-live", NOW))
        .expect("authenticate")
        .expect("live key resolves");
    assert_eq!(owner.key_id, id);
    assert_eq!(owner.last_used_at, None);
    assert_eq!(owner.user.id, "u-1");
    assert_eq!(owner.user.email, "user@example.com");
    assert_eq!(owner.user.name, "Test User");
    // `users.picture` is NULL for the seed → COALESCE yields ''.
    assert_eq!(owner.user.picture, "");
}

#[test]
fn authenticate_rejects_expired_revoked_unknown_and_deleted_owner() {
    let h = harness();
    let repo = repo(&h);

    // Expired yesterday.
    let mut expired = key(&new_id(), "Expired", "hash-expired", CREATED_EARLY);
    expired.expires_at = Some("2026-10-09T12:00:00Z".to_string());
    // Expiring exactly at `NOW` is already dead: the column compares with `>`.
    let mut boundary = key(&new_id(), "Boundary", "hash-boundary", CREATED_EARLY);
    boundary.expires_at = Some(NOW.to_string());
    // NULL expiry never expires.
    let mut never = key(&new_id(), "Never", "hash-never", CREATED_EARLY);
    never.expires_on = None;
    never.expires_at = None;
    // Revoked key.
    let revoked_id = new_id();
    let revoked = key(&revoked_id, "Revoked", "hash-revoked", CREATED_EARLY);

    for input in [expired, boundary, never, revoked] {
        pollster::block_on(repo.insert(input)).expect("insert");
    }

    assert!(pollster::block_on(repo.authenticate("hash-never", NOW))
        .expect("authenticate")
        .is_some());
    for hash in ["hash-expired", "hash-boundary"] {
        assert!(
            pollster::block_on(repo.authenticate(hash, NOW))
                .expect("authenticate")
                .is_none(),
            "{hash} must not authenticate"
        );
    }

    assert!(pollster::block_on(repo.soft_delete(&revoked_id, "u-1", NOW)).expect("revoke"));
    assert!(pollster::block_on(repo.authenticate("hash-revoked", NOW))
        .expect("authenticate")
        .is_none());

    // Unknown hash.
    assert!(pollster::block_on(repo.authenticate("hash-unknown", NOW))
        .expect("authenticate")
        .is_none());

    // Soft-deleting the owner invalidates every one of their keys.
    {
        let conn = h.db.lock().expect("db");
        conn.execute(
            "UPDATE users SET deleted_at = ?1, updated_at = ?1 WHERE id = 'u-1'",
            rusqlite::params![NOW],
        )
        .expect("soft delete user");
    }
    assert!(pollster::block_on(repo.authenticate("hash-never", NOW))
        .expect("authenticate")
        .is_none());
}

#[test]
fn count_active_ignores_revoked_rows() {
    let h = harness();
    let repo = repo(&h);

    let ids = [new_id(), new_id(), new_id()];
    for (index, id) in ids.iter().enumerate() {
        let hash = format!("hash-{index}");
        pollster::block_on(repo.insert(key(id, "Laptop", &hash, CREATED_EARLY))).expect("insert");
    }
    assert_eq!(
        pollster::block_on(repo.count_active("u-1")).expect("count"),
        3
    );

    assert!(pollster::block_on(repo.soft_delete(&ids[1], "u-1", NOW)).expect("revoke"));
    assert_eq!(
        pollster::block_on(repo.count_active("u-1")).expect("count"),
        2
    );
    assert_eq!(
        pollster::block_on(repo.count_active("u-2")).expect("count other"),
        0
    );
}

#[test]
fn soft_delete_is_scoped_to_the_owner() {
    let h = harness();
    let repo = repo(&h);
    let id = new_id();
    pollster::block_on(repo.insert(key(&id, "Laptop", "hash-1", CREATED_EARLY))).expect("insert");

    // Wrong owner: no row changes, and the key keeps working.
    assert!(!pollster::block_on(repo.soft_delete(&id, "u-2", NOW)).expect("revoke other"));
    assert_eq!(
        pollster::block_on(repo.list_by_user_id("u-1"))
            .expect("list")
            .len(),
        1
    );
    assert!(pollster::block_on(repo.authenticate("hash-1", NOW))
        .expect("authenticate")
        .is_some());

    // Right owner: the row is stamped, not deleted, and disappears from reads.
    assert!(pollster::block_on(repo.soft_delete(&id, "u-1", NOW)).expect("revoke"));
    assert!(pollster::block_on(repo.list_by_user_id("u-1"))
        .expect("list")
        .is_empty());
    assert!(pollster::block_on(repo.authenticate("hash-1", NOW))
        .expect("authenticate")
        .is_none());

    // A repeat revoke changes nothing (still reported as "no row changed").
    assert!(!pollster::block_on(repo.soft_delete(&id, "u-1", NOW)).expect("revoke again"));

    // The row survives with both stamps written by the caller's timestamp.
    let conn = h.db.lock().expect("db");
    let stored: Option<ApiKey> =
        query_one(&conn, "SELECT * FROM api_keys WHERE id = ?", &[&id]).expect("read stored");
    let stored = stored.expect("revoked row survives");
    assert_eq!(stored.deleted_at.as_deref(), Some(NOW));
    assert_eq!(stored.updated_at, NOW);
    assert_eq!(stored.created_at, CREATED_EARLY);
}

#[test]
fn mark_used_only_rewrites_a_stale_stamp() {
    let h = harness();
    let repo = repo(&h);
    let null_id = new_id();
    let stale_id = new_id();
    let fresh_id = new_id();
    for (id, hash) in [
        (&null_id, "hash-null"),
        (&stale_id, "hash-stale"),
        (&fresh_id, "hash-fresh"),
    ] {
        pollster::block_on(repo.insert(key(id, "Laptop", hash, CREATED_EARLY))).expect("insert");
    }

    // Seed the STALE row with a 10:00 stamp and the FRESH row with 11:30,
    // both through the NULL branch of the same statement.
    pollster::block_on(repo.mark_used(&stale_id, "2026-10-10T00:00:00Z", "2026-10-10T10:00:00Z"))
        .expect("seed stale stamp");
    pollster::block_on(repo.mark_used(&fresh_id, "2026-10-10T00:00:00Z", "2026-10-10T11:30:00Z"))
        .expect("seed fresh stamp");

    // At NOW with a one-hour cutoff: NULL and 10:00 move, 11:30 does not.
    for id in [&null_id, &stale_id, &fresh_id] {
        pollster::block_on(repo.mark_used(id, CUTOFF, NOW)).expect("mark used");
    }

    let mut stamps: Vec<(String, Option<String>)> = pollster::block_on(repo.list_by_user_id("u-1"))
        .expect("list")
        .into_iter()
        .map(|key| (key.id, key.last_used_at))
        .collect();
    stamps.sort();
    let mut expected = vec![
        (null_id, Some(NOW.to_string())),
        (stale_id, Some(NOW.to_string())),
        (fresh_id, Some("2026-10-10T11:30:00Z".to_string())),
    ];
    expected.sort();
    assert_eq!(stamps, expected);
}
