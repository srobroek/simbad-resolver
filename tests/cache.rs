// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `Cache` behaviour tests for the redb [`Store`], run against BOTH the
//! in-memory and a temp-file store mode (each test is a free `async fn` over a
//! `Store`, and `both_modes!` generates one `#[tokio::test]` per mode). Folds
//! the coverage of the former dashmap and SQLite backends into one suite.

use std::sync::atomic::{AtomicU32, Ordering};

use simbad_resolver::identity::namespace;
use simbad_resolver::normalize::normalize;
use simbad_resolver::{
    AliasKind, BatchDurability, Cache, ObjectType, ResolvedAlias, ResolvedIdentity, Store,
    TargetSource, UpsertOutcome, RANK_EXACT, RANK_PREFIX, RANK_SUBSTRING,
};
use uuid::Uuid;

fn ns() -> Uuid {
    namespace("simbad-resolver-cache.tests")
}

fn m31(source: TargetSource) -> ResolvedIdentity {
    ResolvedIdentity {
        simbad_oid: Some(1_575_544),
        primary_designation: "M 31".to_owned(),
        common_name: Some("Andromeda Galaxy".to_owned()),
        object_type: ObjectType::Galaxy,
        otype_raw: "G".to_owned(),
        ra_deg: 10.684_708,
        dec_deg: 41.268_75,
        v_mag: Some(3.44),
        galdim_majaxis_arcmin: Some(199.53),
        galdim_minaxis_arcmin: Some(70.79),
        galdim_angle_deg: Some(35),
        aliases: vec![
            ResolvedAlias::new("M 31", AliasKind::Designation),
            ResolvedAlias::new("NGC 224", AliasKind::Designation),
            ResolvedAlias::new("Andromeda Galaxy", AliasKind::CommonName),
        ],
        source,
    }
}

fn m101() -> ResolvedIdentity {
    ResolvedIdentity {
        simbad_oid: Some(3_456_789),
        primary_designation: "M 101".to_owned(),
        common_name: Some("Pinwheel Galaxy".to_owned()),
        object_type: ObjectType::Galaxy,
        otype_raw: "G".to_owned(),
        ra_deg: 210.802_42,
        dec_deg: 54.348_95,
        v_mag: Some(7.86),
        galdim_majaxis_arcmin: None,
        galdim_minaxis_arcmin: None,
        galdim_angle_deg: None,
        aliases: vec![
            ResolvedAlias::new("M 101", AliasKind::Designation),
            ResolvedAlias::new("NGC 5457", AliasKind::Designation),
            ResolvedAlias::new("Pinwheel Galaxy", AliasKind::CommonName),
        ],
        source: TargetSource::Seed,
    }
}

async fn seeded(cache: &impl Cache) {
    cache.upsert(&m31(TargetSource::Resolved), &ns()).await.unwrap();
    cache.upsert(&m101(), &ns()).await.unwrap();
}

/// A temp-dir database path unique to this process + call.
fn unique_db_path() -> std::path::PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("simbad-resolver-cache-{}-{n}.redb", std::process::id()))
}

/// For each named `async fn(Store)` test, generate two `#[tokio::test]`s: one
/// over `Store::in_memory()`, one over a temp-file `Store::open(..)`.
macro_rules! both_modes {
    ($($name:ident)*) => {
        $(
            mod $name {
                #[tokio::test]
                async fn in_memory() {
                    super::$name(super::Store::in_memory().expect("in-memory store")).await;
                }
                #[tokio::test]
                async fn file_backed() {
                    let path = super::unique_db_path();
                    super::$name(super::Store::open(&path).expect("file store")).await;
                    let _ = std::fs::remove_file(&path);
                }
            }
        )*
    };
}

both_modes! {
    insert_then_read_by_oid
    read_by_normalized_alias
    dedup_by_oid_updates_single_row
    user_override_is_sticky_against_resolved
    user_override_overwrites_resolved
    resolved_refreshes_existing_resolved_row
    null_oid_dedups_by_derived_id
    get_missing_returns_none
    aliases_rewritten_on_update
    search_blank_query_is_empty
    search_exact_match_ranks_first
    search_prefix_matches_both_ngc
    search_substring_matches_common_name
    search_dedupes_one_hit_per_target
    search_respects_limit
    search_like_wildcards_are_literal
    add_user_alias_is_idempotent
    add_user_alias_rejects_blank_alias
    remove_user_alias_missing_id_returns_false
    list_orders_by_primary_designation
    list_carries_aliases_for_resolved_target
    list_aliases_do_not_cross_contaminate
    upsert_batch_inserts_then_dedups_like_sequential_upsert
}

// ── upsert dedup / precedence ────────────────────────────────────────────────

async fn insert_then_read_by_oid(store: Store) {
    let cache = store.cache();
    let (id, outcome) = cache.upsert(&m31(TargetSource::Resolved), &ns()).await.unwrap();
    assert_eq!(outcome, UpsertOutcome::Inserted);

    let got = cache.get_by_simbad_oid(1_575_544).await.unwrap().unwrap();
    assert_eq!(got.id, id);
    assert_eq!(got.primary_designation, "M 31");
    assert_eq!(got.object_type, ObjectType::Galaxy);
    assert_eq!(got.otype_raw, "G");
    assert_eq!(got.source, TargetSource::Resolved);
    assert_eq!(got.common_name.as_deref(), Some("Andromeda Galaxy"));
    assert_eq!(got.aliases.len(), 3);
}

async fn read_by_normalized_alias(store: Store) {
    let cache = store.cache();
    cache.upsert(&m31(TargetSource::Resolved), &ns()).await.unwrap();

    let got = cache.get_by_normalized(&normalize("NGC 224")).await.unwrap().unwrap();
    assert_eq!(got.primary_designation, "M 31");

    let got2 = cache.get_by_normalized(&normalize("Andromeda Galaxy")).await.unwrap().unwrap();
    assert_eq!(got2.id, got.id);
}

async fn dedup_by_oid_updates_single_row(store: Store) {
    let cache = store.cache();
    let (id1, _) = cache.upsert(&m31(TargetSource::Resolved), &ns()).await.unwrap();

    let mut alt = m31(TargetSource::Resolved);
    "NGC 224".clone_into(&mut alt.primary_designation);
    let (id2, outcome) = cache.upsert(&alt, &ns()).await.unwrap();
    assert_eq!(outcome, UpsertOutcome::Updated);
    assert_eq!(id1, id2, "dedup by oid must keep the same row id");

    assert_eq!(cache.list().await.unwrap().len(), 1);
    let got = cache.get_by_id(id1).await.unwrap().unwrap();
    assert_eq!(got.primary_designation, "NGC 224");
}

async fn user_override_is_sticky_against_resolved(store: Store) {
    let cache = store.cache();
    let (id, _) = cache.upsert(&m31(TargetSource::UserOverride), &ns()).await.unwrap();

    let mut later = m31(TargetSource::Resolved);
    "WRONG".clone_into(&mut later.primary_designation);
    let (id2, outcome) = cache.upsert(&later, &ns()).await.unwrap();
    assert_eq!(outcome, UpsertOutcome::SkippedUserOverride);
    assert_eq!(id, id2);

    let got = cache.get_by_id(id).await.unwrap().unwrap();
    assert_eq!(got.primary_designation, "M 31");
    assert_eq!(got.source, TargetSource::UserOverride);
}

async fn user_override_overwrites_resolved(store: Store) {
    let cache = store.cache();
    cache.upsert(&m31(TargetSource::Resolved), &ns()).await.unwrap();

    let mut over = m31(TargetSource::UserOverride);
    "Andromeda".clone_into(&mut over.primary_designation);
    let (_, outcome) = cache.upsert(&over, &ns()).await.unwrap();
    assert_eq!(outcome, UpsertOutcome::Updated);

    let got = cache.get_by_simbad_oid(1_575_544).await.unwrap().unwrap();
    assert_eq!(got.source, TargetSource::UserOverride);
    assert_eq!(got.primary_designation, "Andromeda");
}

async fn resolved_refreshes_existing_resolved_row(store: Store) {
    let cache = store.cache();
    cache.upsert(&m31(TargetSource::Resolved), &ns()).await.unwrap();

    let mut refreshed = m31(TargetSource::Resolved);
    refreshed.dec_deg = 41.0;
    let (_, outcome) = cache.upsert(&refreshed, &ns()).await.unwrap();
    assert_eq!(outcome, UpsertOutcome::Updated);

    let got = cache.get_by_simbad_oid(1_575_544).await.unwrap().unwrap();
    assert!((got.dec_deg - 41.0).abs() < f64::EPSILON);
}

async fn null_oid_dedups_by_derived_id(store: Store) {
    let cache = store.cache();
    let mut seed = m31(TargetSource::Seed);
    seed.simbad_oid = None;
    let (id1, o1) = cache.upsert(&seed, &ns()).await.unwrap();
    assert_eq!(o1, UpsertOutcome::Inserted);

    let (id2, o2) = cache.upsert(&seed, &ns()).await.unwrap();
    assert_eq!(id1, id2);
    assert_eq!(o2, UpsertOutcome::Updated);
    assert_eq!(cache.list().await.unwrap().len(), 1);
}

async fn get_missing_returns_none(store: Store) {
    let cache = store.cache();
    assert!(cache.get_by_simbad_oid(999).await.unwrap().is_none());
    assert!(cache.get_by_normalized("nothing").await.unwrap().is_none());
    assert!(cache.get_by_id(Uuid::new_v4()).await.unwrap().is_none());
}

async fn aliases_rewritten_on_update(store: Store) {
    let cache = store.cache();
    cache.upsert(&m31(TargetSource::Resolved), &ns()).await.unwrap();

    let mut fewer = m31(TargetSource::Resolved);
    fewer.aliases = vec![ResolvedAlias::new("M 31", AliasKind::Designation)];
    let (id, _) = cache.upsert(&fewer, &ns()).await.unwrap();
    let got = cache.get_by_id(id).await.unwrap().unwrap();
    assert_eq!(got.aliases.len(), 1);
    assert_eq!(got.aliases[0].alias, "M 31");
}

/// `upsert_batch` (the redb backend's single-write-transaction override, or
/// the default per-item loop for any other `Cache` impl) must dedup + report
/// outcomes identically to calling `upsert` once per identity — exercised
/// against both the in-memory and file-backed (real fsync) redb modes.
async fn upsert_batch_inserts_then_dedups_like_sequential_upsert(store: Store) {
    let cache = store.cache();
    let identities = [m31(TargetSource::Resolved), m101()];

    let inserted = cache.upsert_batch(&identities, &ns()).await.unwrap();
    assert_eq!(inserted.len(), 2);
    assert_eq!(inserted[0].1, UpsertOutcome::Inserted);
    assert_eq!(inserted[1].1, UpsertOutcome::Inserted);
    assert_eq!(cache.list().await.unwrap().len(), 2);

    // Re-running the same batch must dedup by oid (each row updated in
    // place), never duplicated — same as re-running `upsert` sequentially.
    let updated = cache.upsert_batch(&identities, &ns()).await.unwrap();
    assert_eq!(updated[0], (inserted[0].0, UpsertOutcome::Updated));
    assert_eq!(updated[1], (inserted[1].0, UpsertOutcome::Updated));
    assert_eq!(cache.list().await.unwrap().len(), 2, "batch dedup must not duplicate rows");
}

/// Regression guard for nightwatch-astro/alm#695's O(n²) defect: bulk-loading
/// N distinct-oid identities through `upsert_batch` used to re-scan the whole
/// (growing) targets table once per entry (measured: 4,000 entries ~124s). A
/// real seed re-warm is ~13k entries; 10k here on a file-backed (real fsync)
/// store, under a generous 30s bound, proves the fix without being tight
/// enough to flake on a slow CI runner.
#[tokio::test]
async fn upsert_batch_of_ten_thousand_completes_well_under_a_generous_bound() {
    let path = unique_db_path();
    let store = Store::open(&path).expect("file store");
    let cache = store.cache();
    let namespace = ns();

    let identities: Vec<ResolvedIdentity> = (0..10_000)
        .map(|i| {
            let idx = u32::try_from(i).expect("10_000 fits in u32");
            let designation = format!("Perf Guard Target {i}");
            ResolvedIdentity {
                simbad_oid: Some(1_000_000 + i64::from(idx)),
                primary_designation: designation.clone(),
                common_name: None,
                object_type: ObjectType::Galaxy,
                otype_raw: "G".to_owned(),
                ra_deg: f64::from(idx % 360),
                dec_deg: 0.0,
                v_mag: None,
                galdim_majaxis_arcmin: None,
                galdim_minaxis_arcmin: None,
                galdim_angle_deg: None,
                aliases: vec![ResolvedAlias::new(designation, AliasKind::Designation)],
                source: TargetSource::Seed,
            }
        })
        .collect();

    let started = std::time::Instant::now();
    let results = cache.upsert_batch(&identities, &namespace).await.unwrap();
    let elapsed = started.elapsed();

    assert_eq!(results.len(), 10_000);
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "10k-entry upsert_batch took {elapsed:?}, expected well under 30s \
         (pre-fix O(n^2) dedup made 4k entries alone take ~124s)"
    );

    let _ = std::fs::remove_file(&path);
}

// ── BatchDurability (nightwatch-astro/alm#695 chunked-warm follow-up) ───────

/// [`Cache::upsert_batch`] against a plain [`Store::open`] (default
/// [`BatchDurability::Durable`]) must still survive a full close + reopen
/// with no other action needed — the new durability knob must not change
/// this pre-existing method's behavior for callers who never opt in.
#[tokio::test]
async fn default_upsert_batch_path_is_still_fully_durable() {
    let path = unique_db_path();
    {
        let store = Store::open(&path).expect("file store");
        let cache = store.cache();
        cache.upsert_batch(&[m31(TargetSource::Resolved), m101()], &ns()).await.unwrap();
    } // full close: drops the Store (and its redb::Database)

    let reopened = Store::open(&path).expect("reopen file store");
    let rows = reopened.cache().list().await.unwrap();
    assert_eq!(rows.len(), 2, "plain upsert_batch must still be durable with no follow-up commit");

    let _ = std::fs::remove_file(&path);
}

/// An `Eventual`-durability `upsert_batch` commit is visible to reads against
/// the same open `Store` right away — durability only affects crash
/// survival, not in-process visibility.
#[tokio::test]
async fn eventual_batch_entries_are_immediately_queryable() {
    let path = unique_db_path();
    let store = Store::open_with(&path, BatchDurability::Eventual).expect("file store");
    let cache = store.cache();

    let results = cache.upsert_batch(&[m31(TargetSource::Resolved)], &ns()).await.unwrap();
    assert_eq!(results[0].1, UpsertOutcome::Inserted);

    let got = cache.get_by_id(results[0].0).await.unwrap();
    assert_eq!(got.map(|t| t.primary_designation), Some("M 31".to_owned()));

    let _ = std::fs::remove_file(&path);
}

/// The core chunked-load contract: with the store opened `Eventual`, N
/// `upsert_batch` chunks (no fsync each) followed by exactly one
/// [`RedbCache::flush`] must persist *every* chunk — not just the last one —
/// across a full close + reopen. Proven with 2 chunks; redb commits are
/// cumulative, so this generalizes to any chunk count.
#[tokio::test]
async fn flush_persists_every_prior_eventual_batch_across_reopen() {
    let path = unique_db_path();
    let namespace = ns();
    {
        let store = Store::open_with(&path, BatchDurability::Eventual).expect("file store");
        let cache = store.cache();
        cache.upsert_batch(&[m31(TargetSource::Resolved)], &namespace).await.unwrap();
        cache.upsert_batch(&[m101()], &namespace).await.unwrap();
        cache.flush().await.unwrap();
    } // full close, no further commits after flush()

    let reopened = Store::open_with(&path, BatchDurability::Eventual).expect("reopen file store");
    let rows = reopened.cache().list().await.unwrap();
    assert_eq!(rows.len(), 2, "flush() must persist both prior Eventual chunks, not just the last");
    assert!(rows.iter().any(|r| r.primary_designation == "M 31"));
    assert!(rows.iter().any(|r| r.primary_designation == "M 101"));

    let _ = std::fs::remove_file(&path);
}

/// Single-item [`Cache::upsert`] must stay durable regardless of the store's
/// configured bulk `BatchDurability` — point writes are never traded away,
/// only `upsert_batch` opts into `Eventual`.
#[tokio::test]
async fn single_item_upsert_stays_durable_even_under_eventual_bulk_policy() {
    let path = unique_db_path();
    {
        let store = Store::open_with(&path, BatchDurability::Eventual).expect("file store");
        let cache = store.cache();
        cache.upsert(&m31(TargetSource::Resolved), &ns()).await.unwrap();
    } // full close, no flush() and no upsert_batch call at all

    let reopened = Store::open_with(&path, BatchDurability::Eventual).expect("reopen file store");
    let rows = reopened.cache().list().await.unwrap();
    assert_eq!(
        rows.len(),
        1,
        "a single-item upsert must be durable even when bulk_durability is Eventual"
    );

    let _ = std::fs::remove_file(&path);
}

// ── search ranking ───────────────────────────────────────────────────────────

async fn search_blank_query_is_empty(store: Store) {
    let cache = store.cache();
    seeded(&cache).await;
    assert!(cache.search("   ", 20).await.unwrap().is_empty());
    assert!(cache.search("M31", 0).await.unwrap().is_empty());
}

async fn search_exact_match_ranks_first(store: Store) {
    let cache = store.cache();
    seeded(&cache).await;
    let hits = cache.search("NGC 5457", 20).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].rank, RANK_EXACT);
    assert_eq!(hits[0].target.primary_designation, "M 101");
    assert_eq!(hits[0].matched_alias, "NGC 5457");
}

async fn search_prefix_matches_both_ngc(store: Store) {
    let cache = store.cache();
    seeded(&cache).await;
    let hits = cache.search("NGC", 20).await.unwrap();
    assert_eq!(hits.len(), 2);
    assert!(hits.iter().all(|h| h.rank == RANK_PREFIX));
}

async fn search_substring_matches_common_name(store: Store) {
    let cache = store.cache();
    seeded(&cache).await;
    let hits = cache.search("galaxy", 20).await.unwrap();
    assert_eq!(hits.len(), 2);
    assert!(hits.iter().all(|h| h.rank == RANK_SUBSTRING));
}

async fn search_dedupes_one_hit_per_target(store: Store) {
    let cache = store.cache();
    let mut t = m31(TargetSource::Resolved);
    t.aliases = vec![
        ResolvedAlias::new("Andromeda", AliasKind::CommonName),
        ResolvedAlias::new("Andromeda Galaxy", AliasKind::CommonName),
    ];
    cache.upsert(&t, &ns()).await.unwrap();

    let hits = cache.search("andromeda", 20).await.unwrap();
    assert_eq!(hits.len(), 1, "one canonical target despite two matching aliases");
    assert_eq!(hits[0].rank, RANK_EXACT);
    assert_eq!(hits[0].matched_alias, "Andromeda");
}

async fn search_respects_limit(store: Store) {
    let cache = store.cache();
    seeded(&cache).await;
    let hits = cache.search("galaxy", 1).await.unwrap();
    assert_eq!(hits.len(), 1);
}

async fn search_like_wildcards_are_literal(store: Store) {
    let cache = store.cache();
    seeded(&cache).await;
    // Punctuation normalizes away, so neither matches any stored alias.
    assert!(cache.search("%", 20).await.unwrap().is_empty());
    assert!(cache.search("_", 20).await.unwrap().is_empty());
}

// ── user aliases ─────────────────────────────────────────────────────────────

async fn add_user_alias_is_idempotent(store: Store) {
    let cache = store.cache();
    let (id, _) = cache.upsert(&m31(TargetSource::Resolved), &ns()).await.unwrap();

    assert!(cache.add_user_alias(id, "M31 favorite").await.unwrap());
    assert!(!cache.add_user_alias(id, "M31 favorite").await.unwrap(), "idempotent re-add");

    let got = cache.get_by_id(id).await.unwrap().unwrap();
    assert_eq!(got.aliases.len(), 4);
    assert!(got.aliases.iter().any(|a| a.kind == AliasKind::User && a.alias == "M31 favorite"));
}

async fn add_user_alias_rejects_blank_alias(store: Store) {
    let cache = store.cache();
    let (id, _) = cache.upsert(&m31(TargetSource::Resolved), &ns()).await.unwrap();
    assert!(!cache.add_user_alias(id, "   ").await.unwrap());
    assert!(!cache.add_user_alias(id, "").await.unwrap());
    let got = cache.get_by_id(id).await.unwrap().unwrap();
    assert!(got.aliases.iter().all(|a| a.kind != AliasKind::User));
}

async fn remove_user_alias_missing_id_returns_false(store: Store) {
    let cache = store.cache();
    assert!(!cache.remove_user_alias("no-such-id").await.unwrap());
}

// ── list ───────────────────────────────────────────────────────────────────

async fn list_orders_by_primary_designation(store: Store) {
    let cache = store.cache();
    seeded(&cache).await;
    let rows = cache.list().await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].primary_designation, "M 101");
    assert_eq!(rows[1].primary_designation, "M 31");
}

async fn list_carries_aliases_for_resolved_target(store: Store) {
    let cache = store.cache();
    cache.upsert(&m31(TargetSource::Resolved), &ns()).await.unwrap();
    let rows = cache.list().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].primary_designation, "M 31");
    assert_eq!(rows[0].aliases.len(), 3);
}

async fn list_aliases_do_not_cross_contaminate(store: Store) {
    let cache = store.cache();
    seeded(&cache).await;
    let rows = cache.list().await.unwrap();
    assert_eq!(rows.len(), 2);

    let m31_row = rows.iter().find(|r| r.primary_designation == "M 31").unwrap();
    let m101_row = rows.iter().find(|r| r.primary_designation == "M 101").unwrap();
    assert_eq!(m31_row.aliases.len(), 3);
    assert_eq!(m101_row.aliases.len(), 3);
    assert!(!m31_row.aliases.iter().any(|a| a.alias == "NGC 5457"));
    assert!(!m101_row.aliases.iter().any(|a| a.alias == "NGC 224"));
}
