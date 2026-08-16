// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Facade integration tests (offline, via FakeResolver + redb backends).
//!
//! Covers quickstart scenarios S1 (resolve + cache-first), S3 (sticky override),
//! and S4 (batch drain semantics), plus backend substitutability across the
//! in-memory and file-backed redb store modes.

use std::sync::atomic::{AtomicU32, Ordering};

use simbad_resolver::{
    AliasKind, BatchResolver, CacheBackend, FakeResolver, ObjectType, PendingState, Resolution,
    ResolveError, ResolvedAlias, ResolvedIdentity, ResolverConfig, SimbadResolver, Store,
    TargetSource, UnresolvedReason, RANK_FUZZY,
};

fn m31() -> ResolvedIdentity {
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
        source: TargetSource::Resolved,
    }
}

fn store() -> Store {
    Store::in_memory().expect("in-memory store")
}

fn facade(resolver: FakeResolver) -> SimbadResolver<FakeResolver> {
    SimbadResolver::new(resolver, CacheBackend::InMemory, ResolverConfig::new("test.targets"))
        .expect("in-memory facade")
}

#[tokio::test]
async fn resolves_and_caches_dedup_by_alias() {
    let f = facade(FakeResolver::new().with_response("M 31", m31()));

    // First resolve: online (one resolver call), returns the canonical identity.
    let got = f.resolve("M31").await.unwrap();
    let Resolution::Resolved(t) = got else { panic!("expected resolved, got {got:?}") };
    assert_eq!(t.primary_designation, "M 31");
    assert_eq!(t.object_type, ObjectType::Galaxy);
    assert_eq!(t.otype_raw, "G");

    // Resolving an alias of the SAME object is a cache hit (no second network call).
    let got2 = f.resolve("NGC 224").await.unwrap();
    let Resolution::Resolved(t2) = got2 else { panic!("expected resolved") };
    assert_eq!(t2.id, t.id, "aliases collapse onto one canonical target");
    assert_eq!(f.resolver().call_count(), 1, "second resolve served from cache");
}

#[tokio::test]
async fn unknown_query_is_unresolved_unknown() {
    let f = facade(FakeResolver::new()); // default: NotFound
    match f.resolve("definitely-not-an-object").await.unwrap() {
        Resolution::Unresolved { reason, .. } => assert_eq!(reason, UnresolvedReason::Unknown),
        Resolution::Resolved(t) => panic!("expected unresolved unknown, got resolved {t:?}"),
    }
}

#[tokio::test]
async fn ambiguous_query_is_unresolved_ambiguous() {
    let f =
        facade(FakeResolver::new().with_error(
            "cluster",
            ResolveError::Ambiguous { query: "cluster".to_owned(), count: 3 },
        ));
    match f.resolve("cluster").await.unwrap() {
        Resolution::Unresolved { reason, .. } => assert_eq!(reason, UnresolvedReason::Ambiguous),
        Resolution::Resolved(t) => panic!("expected ambiguous, got resolved {t:?}"),
    }
}

#[tokio::test]
async fn transient_failure_degrades_to_offline() {
    let f =
        facade(FakeResolver::new().with_default_error(ResolveError::Network("down".to_owned())));
    match f.resolve("M 31").await.unwrap() {
        Resolution::Unresolved { reason, .. } => assert_eq!(reason, UnresolvedReason::Offline),
        Resolution::Resolved(t) => panic!("expected offline, got resolved {t:?}"),
    }
}

#[tokio::test]
async fn online_disabled_never_calls_resolver_but_cache_still_works() {
    // Seed the cache while online, then disable online (sharing the same store).
    let cache = store().cache();
    let resolver = FakeResolver::new().with_response("M 31", m31());
    let online = SimbadResolver::new(
        resolver,
        CacheBackend::custom(cache.clone()),
        ResolverConfig::new("test.targets"),
    )
    .unwrap();
    online.resolve("M 31").await.unwrap();

    let offline_resolver = FakeResolver::new().with_response("M 31", m31());
    let offline = SimbadResolver::new(
        offline_resolver,
        CacheBackend::custom(cache),
        ResolverConfig::new("test.targets").with_online(false),
    )
    .unwrap();
    // Cached object still resolves with no resolver call.
    assert!(matches!(offline.resolve("NGC 224").await.unwrap(), Resolution::Resolved(_)));
    // Unknown object → offline (not a network attempt).
    assert!(matches!(
        offline.resolve("unknown").await.unwrap(),
        Resolution::Unresolved { reason: UnresolvedReason::Offline, .. }
    ));
    assert_eq!(offline.resolver().call_count(), 0, "online disabled → no resolver calls");
}

#[tokio::test]
async fn apply_override_is_sticky_user_override() {
    let f = facade(FakeResolver::new().with_response("M 31", m31()));
    let Resolution::Resolved(t) = f.resolve("M 31").await.unwrap() else { panic!() };
    assert_eq!(t.source, TargetSource::Resolved);

    let overridden = f.apply_override(t.id, "My Andromeda").await.unwrap().unwrap();
    assert_eq!(overridden.source, TargetSource::UserOverride);
    assert!(overridden.aliases.iter().any(|a| a.alias == "My Andromeda"));

    // Unknown target id → None.
    assert!(f.apply_override(uuid::Uuid::new_v4(), "x").await.unwrap().is_none());
}

#[tokio::test]
async fn caldwell_query_translates_and_binds_alias() {
    // C 14 → NGC 869 (Double Cluster). The fake resolves NGC 869; the facade
    // translates C 14 → NGC 869, resolves, and binds "C 14" as an alias.
    let ngc869 = ResolvedIdentity {
        simbad_oid: Some(10_001),
        primary_designation: "NGC 869".to_owned(),
        common_name: Some("Double Cluster".to_owned()),
        object_type: ObjectType::OpenCluster,
        otype_raw: "OpC".to_owned(),
        ra_deg: 34.75,
        dec_deg: 57.13,
        v_mag: None,
        galdim_majaxis_arcmin: None,
        galdim_minaxis_arcmin: None,
        galdim_angle_deg: None,
        aliases: vec![ResolvedAlias::new("NGC 869", AliasKind::Designation)],
        source: TargetSource::Resolved,
    };
    let f = facade(FakeResolver::new().with_response("NGC 869", ngc869));
    let Resolution::Resolved(t) = f.resolve("C 14").await.unwrap() else { panic!() };
    assert_eq!(t.primary_designation, "NGC 869");
    assert!(t.aliases.iter().any(|a| a.alias == "C 14"), "original Caldwell bound as alias");
    // A subsequent C 14 lookup is now a cache hit.
    assert!(matches!(f.resolve("C 14").await.unwrap(), Resolution::Resolved(_)));
}

#[tokio::test]
async fn batch_drain_resolves_misses_and_retries_transient() {
    let resolver = FakeResolver::new()
        .with_response("M 31", m31())
        .with_error("timeout-one", ResolveError::Timeout(10));
    let s = store();
    // default (unregistered) → NotFound (content miss)
    let batch =
        BatchResolver::new(resolver, s.cache(), s.queue(), ResolverConfig::new("test.targets"));

    batch.enqueue("img1", "M 31").await.unwrap();
    batch.enqueue("img2", "unknown-object").await.unwrap();
    batch.enqueue("img3", "NGC 224").await.unwrap(); // alias of M31 → cache hit after img1
    batch.enqueue("img4", "timeout-one").await.unwrap();

    let summary = batch.drain().await.unwrap();
    assert_eq!(summary.resolved, 2, "img1 (M 31) + img3 (NGC 224 via cache) resolved");
    assert_eq!(summary.unresolved, 1, "img2 unknown → unresolved");
    assert_eq!(summary.still_pending, 1, "img4 transient → still pending");

    // Transient item retains its pending state and attempt budget (attempts == 0).
    let q = batch.queue();
    let pending = q.get("img4").await.unwrap().unwrap();
    assert_eq!(pending.state, PendingState::Pending);
    assert_eq!(pending.attempts, 0, "transient failure does not consume an attempt");

    let unresolved = q.get("img2").await.unwrap().unwrap();
    assert_eq!(unresolved.state, PendingState::Unresolved);
    assert_eq!(unresolved.attempts, 1, "content miss consumes an attempt");
}

#[tokio::test]
async fn search_is_network_free_and_delegates_to_cache() {
    let f = facade(FakeResolver::new().with_response("M 31", m31()));
    // Seed one target online (one resolver call).
    f.resolve("M 31").await.unwrap();
    assert_eq!(f.resolver().call_count(), 1);

    // Typeahead search consults the cache only — SC-001: no network resolution.
    let hits = f.search("andromeda", 10).await.unwrap();
    assert!(!hits.is_empty());
    assert!(hits.iter().any(|h| h.target.primary_designation == "M 31"));
    assert_eq!(f.resolver().call_count(), 1, "search performs no network resolution");

    // A blank query or a zero limit yields no hits.
    assert!(f.search("   ", 10).await.unwrap().is_empty());
    assert!(f.search("M 31", 0).await.unwrap().is_empty());
}

#[tokio::test]
async fn batch_drain_spans_multiple_rounds_when_batch_size_is_small() {
    let resolver = FakeResolver::new().with_response("M 31", m31());
    let s = store();
    // with_batch_size(0) clamps to 1 -> one item per round, forcing >1 round.
    let batch =
        BatchResolver::new(resolver, s.cache(), s.queue(), ResolverConfig::new("test.targets"))
            .with_batch_size(0);

    batch.enqueue("img1", "M 31").await.unwrap();
    batch.enqueue("img2", "NGC 224").await.unwrap(); // alias of M31 -> cache hit
    batch.enqueue("img3", "Andromeda Galaxy").await.unwrap(); // common name -> cache hit

    let summary = batch.drain().await.unwrap();
    assert_eq!(summary.resolved, 3, "all three drained across single-item rounds");
    assert_eq!(summary.unresolved, 0);
    assert_eq!(summary.still_pending, 0);
    assert_eq!(batch.queue().pending_count().await.unwrap(), 0);
}

#[tokio::test]
async fn caldwell_without_a_designation_is_unresolved_unknown() {
    // C 99 (the Coalsack) has no NGC/IC designation in the Caldwell map, so the
    // facade returns Unresolved{Unknown} without ever calling the resolver.
    let f = facade(FakeResolver::new());
    match f.resolve("C 99").await.unwrap() {
        Resolution::Unresolved { reason, .. } => assert_eq!(reason, UnresolvedReason::Unknown),
        Resolution::Resolved(t) => panic!("expected unresolved unknown, got resolved {t:?}"),
    }
    assert_eq!(f.resolver().call_count(), 0, "no network call for an untranslatable Caldwell id");
}

/// SC-006: the SAME facade code path works against a durable, file-backed
/// store, unchanged — demonstrating store-mode substitutability.
#[tokio::test]
async fn resolve_flow_is_backend_agnostic_file_backed() {
    let path = unique_db_path();
    let resolver = FakeResolver::new().with_response("M 31", m31());
    let f = SimbadResolver::new(
        resolver,
        CacheBackend::file(&path),
        ResolverConfig::new("test.targets"),
    )
    .expect("open file-backed facade");

    let Resolution::Resolved(t) = f.resolve("M31").await.unwrap() else {
        panic!("expected resolved")
    };
    assert_eq!(t.primary_designation, "M 31");
    assert_eq!(t.object_type, ObjectType::Galaxy);

    // Alias dedup + cache-first hold on the file store exactly as in memory.
    let Resolution::Resolved(t2) = f.resolve("NGC 224").await.unwrap() else { panic!() };
    assert_eq!(t2.id, t.id);
    assert_eq!(f.resolver().call_count(), 1, "second resolve served from the file-backed cache");

    drop(f);
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn fuzzy_search_matches_reordered_tokens_when_enabled() {
    // "galaxy andromeda" is neither an exact, prefix, nor substring alias of M31,
    // but its token set equals "Andromeda Galaxy" — so it matches via the fuzzy tier.
    let f = SimbadResolver::new(
        FakeResolver::new().with_response("M 31", m31()),
        CacheBackend::InMemory,
        ResolverConfig::new("test.targets").with_fuzzy(0.5),
    )
    .unwrap();
    f.resolve("M 31").await.unwrap();

    let hits = f.search("galaxy andromeda", 10).await.unwrap();
    assert_eq!(hits.len(), 1, "reordered tokens matched via the fuzzy tier");
    assert_eq!(hits[0].target.primary_designation, "M 31");
    assert_eq!(hits[0].rank, RANK_FUZZY);
}

#[tokio::test]
async fn fuzzy_search_is_off_by_default() {
    // Same query, but no with_fuzzy(): exact/prefix/substring only → no hit.
    let f = facade(FakeResolver::new().with_response("M 31", m31()));
    f.resolve("M 31").await.unwrap();
    assert!(f.search("galaxy andromeda", 10).await.unwrap().is_empty());
}

#[tokio::test]
async fn fuzzy_config_does_not_leak_into_resolve() {
    // Seed M31 online, then reuse the same store offline with fuzzy enabled.
    let cache = store().cache();
    let online = SimbadResolver::new(
        FakeResolver::new().with_response("M 31", m31()),
        CacheBackend::custom(cache.clone()),
        ResolverConfig::new("test.targets"),
    )
    .unwrap();
    online.resolve("M 31").await.unwrap();

    let offline = SimbadResolver::new(
        FakeResolver::new(),
        CacheBackend::custom(cache),
        ResolverConfig::new("test.targets").with_online(false).with_fuzzy(0.1),
    )
    .unwrap();
    // resolve() stays exact: a fuzzy-similar query is NOT resolved from the cache.
    assert!(matches!(
        offline.resolve("galaxy andromeda").await.unwrap(),
        Resolution::Unresolved { reason: UnresolvedReason::Offline, .. }
    ));
    // ...yet the same query IS surfaced by fuzzy search.
    let hits = offline.search("galaxy andromeda", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].rank, RANK_FUZZY);
}

/// A temp-dir database path unique to this process + call, so parallel tests
/// never share a file.
fn unique_db_path() -> std::path::PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("simbad-resolver-facade-{}-{n}.redb", std::process::id()))
}
