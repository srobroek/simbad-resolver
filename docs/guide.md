# Guide

A task-oriented walkthrough of `simbad-resolver`. For the full API reference,
see [docs.rs/simbad-resolver](https://docs.rs/simbad-resolver). The examples
below use `M 31` (the Andromeda Galaxy) and `Vega` as the canonical fixtures —
the same two objects the crate's own doctests and `examples/` use.

## Install

```toml
[dependencies]
simbad-resolver = "0.2"
```

## Resolve a name from SIMBAD

[`TapResolver`](https://docs.rs/simbad-resolver/latest/simbad_resolver/struct.TapResolver.html)
queries the live SIMBAD TAP endpoint, so this snippet needs network access and
is `no_run`:

```rust,no_run
use simbad_resolver::{CacheBackend, Resolution, ResolverConfig, SimbadResolver, TapResolver};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let resolver = TapResolver::with_defaults()?;
// Ephemeral cache; `CacheBackend::file("targets.redb")` persists across restarts.
let facade =
    SimbadResolver::new(resolver, CacheBackend::InMemory, ResolverConfig::new("your.namespace"))?;

match facade.resolve("M31").await? {
    Resolution::Resolved(target) => {
        println!("{} at ({}, {})", target.primary_designation, target.ra_deg, target.dec_deg);
    }
    Resolution::Unresolved { reason, .. } => println!("unresolved: {reason:?}"),
}
# Ok(())
# }
```

[`SimbadResolver::resolve`](https://docs.rs/simbad-resolver/latest/simbad_resolver/struct.SimbadResolver.html#method.resolve)
is cache-first: a query already in the cache never reaches the resolver. That
makes it possible to exercise the whole facade without the network, which is
what the rest of this guide (and the crate's test suite) does.

The `reason` on
[`Resolution::Unresolved`](https://docs.rs/simbad-resolver/latest/simbad_resolver/enum.Resolution.html)
is an
[`UnresolvedReason`](https://docs.rs/simbad-resolver/latest/simbad_resolver/enum.UnresolvedReason.html)
that tells the caller what to do next: `Offline` (backend unreachable, timed
out, or disabled — retry later; cached objects still resolve), `Unknown` (no
such object — give up), or `Ambiguous` (the query maps to several distinct
objects — disambiguate it).

## Test without the network

Two options avoid live SIMBAD calls:

- [`OfflineResolver`](https://docs.rs/simbad-resolver/latest/simbad_resolver/struct.OfflineResolver.html)
  — a zero-cost [`Resolver`](https://docs.rs/simbad-resolver/latest/simbad_resolver/trait.Resolver.html)
  that always returns
  [`ResolveError::Disabled`](https://docs.rs/simbad-resolver/latest/simbad_resolver/enum.ResolveError.html).
  Combine it with a pre-seeded cache to exercise cache-first resolution with no
  feature flag required (used below).
- [`FakeResolver`](https://docs.rs/simbad-resolver/latest/simbad_resolver/struct.FakeResolver.html)
  — an in-memory test double with canned responses/errors, gated behind the
  `test-util` feature (add `simbad-resolver = { version = "0.2", features = ["test-util"] }`
  to `[dev-dependencies]`).

This example is fully runnable — it seeds the cache directly with the `M 31`
fixture, then resolves it through `OfflineResolver` without any network call:

```rust
use simbad_resolver::{
    AliasKind, CacheBackend, ObjectType, OfflineResolver, Resolution, ResolvedAlias,
    ResolvedIdentity, ResolverConfig, SimbadResolver, TargetSource,
};

# async fn run() -> Result<(), simbad_resolver::Error> {
let config = ResolverConfig::new("guide.example");
let namespace = config.namespace;
let facade = SimbadResolver::new(OfflineResolver, CacheBackend::InMemory, config)?;

let m31 = ResolvedIdentity {
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
    aliases: vec![ResolvedAlias::new("M 31", AliasKind::Designation)],
    source: TargetSource::Seed,
};
facade.cache().upsert(&m31, &namespace).await?;

match facade.resolve("M31").await? {
    Resolution::Resolved(target) => assert_eq!(target.primary_designation, "M 31"),
    Resolution::Unresolved { .. } => unreachable!("seeded above"),
}
# Ok(())
# }
```

`examples/resolve_offline.rs` runs the same pattern as a standalone binary:
`cargo run --example resolve_offline`.

With the `test-util` feature enabled, `FakeResolver` gives the same result
without touching the cache directly — it stands in for the resolver, so
`resolve` reaches it on the cache miss instead of the network:

```rust
use simbad_resolver::{
    AliasKind, CacheBackend, FakeResolver, ObjectType, Resolution, ResolvedAlias, ResolvedIdentity,
    ResolverConfig, SimbadResolver, TargetSource,
};

# async fn run() -> Result<(), simbad_resolver::Error> {
let vega = ResolvedIdentity {
    simbad_oid: Some(3_170_642),
    primary_designation: "* alf Lyr".to_owned(),
    common_name: Some("Vega".to_owned()),
    object_type: ObjectType::Other, // SIMBAD otype "*" (a plain star) falls outside the closed set
    otype_raw: "*".to_owned(),
    ra_deg: 279.234_735,
    dec_deg: 38.783_689,
    v_mag: Some(0.03),
    galdim_majaxis_arcmin: None,
    galdim_minaxis_arcmin: None,
    galdim_angle_deg: None,
    aliases: vec![ResolvedAlias::new("Vega", AliasKind::CommonName)],
    source: TargetSource::Resolved,
};
let resolver = FakeResolver::new().with_response("Vega", vega);
let facade =
    SimbadResolver::new(resolver, CacheBackend::InMemory, ResolverConfig::new("guide.example"))?;
match facade.resolve("Vega").await? {
    Resolution::Resolved(target) => assert_eq!(target.primary_designation, "* alf Lyr"),
    Resolution::Unresolved { .. } => unreachable!("FakeResolver has a canned Vega response"),
}
# Ok(())
# }
```

## Typeahead search

[`SimbadResolver::search`](https://docs.rs/simbad-resolver/latest/simbad_resolver/struct.SimbadResolver.html#method.search)
is a local, network-free lookup over cached aliases, ranked exact > prefix >
substring. It never calls the resolver, so — like `resolve` on a cache hit — it
runs offline once the cache is seeded:

```rust
use simbad_resolver::{
    AliasKind, CacheBackend, ObjectType, OfflineResolver, ResolvedAlias, ResolvedIdentity,
    ResolverConfig, SimbadResolver, TargetSource,
};

# async fn run() -> Result<(), simbad_resolver::Error> {
let config = ResolverConfig::new("guide.example");
let namespace = config.namespace;
let facade = SimbadResolver::new(OfflineResolver, CacheBackend::InMemory, config)?;

let m31 = ResolvedIdentity {
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
        ResolvedAlias::new("Andromeda Galaxy", AliasKind::CommonName),
    ],
    source: TargetSource::Seed,
};
facade.cache().upsert(&m31, &namespace).await?;

// Prefix match on the "M 31" alias.
let hits = facade.search("M 3", 5).await?;
assert_eq!(hits[0].target.primary_designation, "M 31");
# Ok(())
# }
```

To also match near-misses (typos, reordered tokens), enable fuzzy matching with
[`ResolverConfig::with_fuzzy`](https://docs.rs/simbad-resolver/latest/simbad_resolver/struct.ResolverConfig.html#method.with_fuzzy);
those hits land in the
[`RANK_FUZZY`](https://docs.rs/simbad-resolver/latest/simbad_resolver/constant.RANK_FUZZY.html)
tier, after the exact/prefix/substring ranks.

## Broader name coverage with Sesame

[`SesameResolver`](https://docs.rs/simbad-resolver/latest/simbad_resolver/struct.SesameResolver.html)
resolves a wider range of names than TAP (it aggregates SIMBAD, NED, and
VizieR), at the cost of coarser output — no `simbad_oid`, and no alias set
unless you attach a `TapResolver` as an
[enricher](https://docs.rs/simbad-resolver/latest/simbad_resolver/struct.SesameResolver.html#method.with_enricher).
This also needs network access:

```rust,no_run
use std::sync::Arc;

use simbad_resolver::{Resolver, SesameResolver, TapResolver};

# async fn run() -> Result<(), simbad_resolver::ResolveError> {
let enricher: Arc<dyn Resolver> = Arc::new(TapResolver::with_defaults()?);
let resolver = SesameResolver::new().with_enricher(enricher);
let identity = resolver.resolve("Vega").await?;
println!("{} @ ({}, {})", identity.primary_designation, identity.ra_deg, identity.dec_deg);
# Ok(())
# }
```

## Batch-resolve many names

[`BatchResolver`](https://docs.rs/simbad-resolver/latest/simbad_resolver/struct.BatchResolver.html)
drains a durable queue cache-first then online, distinguishing transient
failures (retried later) from content misses (marked unresolved).

It takes a cache and a queue as separate arguments. Both come from one
[`Store`](https://docs.rs/simbad-resolver/latest/simbad_resolver/struct.Store.html)
— an open redb database.
[`Store::in_memory`](https://docs.rs/simbad-resolver/latest/simbad_resolver/struct.Store.html#method.in_memory)
(ephemeral) and
[`Store::open`](https://docs.rs/simbad-resolver/latest/simbad_resolver/struct.Store.html#method.open)
(file-backed) each hand out a
[`.cache()`](https://docs.rs/simbad-resolver/latest/simbad_resolver/struct.Store.html#method.cache)
and a
[`.queue()`](https://docs.rs/simbad-resolver/latest/simbad_resolver/struct.Store.html#method.queue)
view over that same database. Passing one `Store`'s `.cache()` to both a
`BatchResolver` and a `SimbadResolver` (via
[`CacheBackend::custom`](https://docs.rs/simbad-resolver/latest/simbad_resolver/enum.CacheBackend.html#method.custom))
makes the facade and the batch resolver operate over the same rows.

This example seeds the cache first, so `drain()` resolves `M 31` without a
resolver call:

```rust
use simbad_resolver::{
    AliasKind, BatchResolver, Cache, ObjectType, OfflineResolver, ResolvedAlias, ResolvedIdentity,
    ResolverConfig, Store, TargetSource,
};

# async fn run() -> Result<(), simbad_resolver::Error> {
let store = Store::in_memory()?;
let config = ResolverConfig::new("guide.example");
let namespace = config.namespace;

let m31 = ResolvedIdentity {
    simbad_oid: Some(1_575_544),
    primary_designation: "M 31".to_owned(),
    common_name: None,
    object_type: ObjectType::Galaxy,
    otype_raw: "G".to_owned(),
    ra_deg: 10.684_708,
    dec_deg: 41.268_75,
    v_mag: Some(3.44),
    galdim_majaxis_arcmin: Some(199.53),
    galdim_minaxis_arcmin: Some(70.79),
    galdim_angle_deg: Some(35),
    aliases: vec![ResolvedAlias::new("M 31", AliasKind::Designation)],
    source: TargetSource::Seed,
};
store.cache().upsert(&m31, &namespace).await?;

let batch = BatchResolver::new(OfflineResolver, store.cache(), store.queue(), config)
    .with_batch_size(8);
batch.enqueue("job-1", "M31").await?;

let summary = batch.drain().await?;
assert_eq!(summary.resolved, 1);
# Ok(())
# }
```

## Pin a target with a user override

[`SimbadResolver::apply_override`](https://docs.rs/simbad-resolver/latest/simbad_resolver/struct.SimbadResolver.html#method.apply_override)
binds a chosen canonical target as authoritative — it adds an alias and marks
the row sticky (`source = user-override`), so a later re-resolve does not
overwrite it. It returns the updated target, or `None` if the id is unknown.
This example seeds `M 31`, then pins it under a custom alias, all offline:

```rust
use simbad_resolver::{
    AliasKind, CacheBackend, ObjectType, OfflineResolver, ResolvedAlias, ResolvedIdentity,
    ResolverConfig, SimbadResolver, TargetSource,
};

# async fn run() -> Result<(), simbad_resolver::Error> {
let config = ResolverConfig::new("guide.example");
let namespace = config.namespace;
let facade = SimbadResolver::new(OfflineResolver, CacheBackend::InMemory, config)?;

let m31 = ResolvedIdentity {
    simbad_oid: Some(1_575_544),
    primary_designation: "M 31".to_owned(),
    common_name: None,
    object_type: ObjectType::Galaxy,
    otype_raw: "G".to_owned(),
    ra_deg: 10.684_708,
    dec_deg: 41.268_75,
    v_mag: Some(3.44),
    galdim_majaxis_arcmin: Some(199.53),
    galdim_minaxis_arcmin: Some(70.79),
    galdim_angle_deg: Some(35),
    aliases: vec![ResolvedAlias::new("M 31", AliasKind::Designation)],
    source: TargetSource::Seed,
};
let (id, _) = facade.cache().upsert(&m31, &namespace).await?;

let pinned = facade.apply_override(id, "My Andromeda").await?.expect("id is known");
assert_eq!(pinned.source, TargetSource::UserOverride);
assert!(pinned.aliases.iter().any(|a| a.alias == "My Andromeda"));
# Ok(())
# }
```

## Typed coordinates

[`ResolvedIdentity`](https://docs.rs/simbad-resolver/latest/simbad_resolver/struct.ResolvedIdentity.html)
and
[`CachedTarget`](https://docs.rs/simbad-resolver/latest/simbad_resolver/struct.CachedTarget.html)
keep `ra_deg`/`dec_deg` as the canonical `f64` storage and also expose a typed
[`skymath::Equatorial`](https://docs.rs/skymath/latest/skymath/struct.Equatorial.html)
accessor:

```rust
use simbad_resolver::{AliasKind, ObjectType, ResolvedAlias, ResolvedIdentity, TargetSource};

let m31 = ResolvedIdentity {
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
    aliases: vec![ResolvedAlias::new("M 31", AliasKind::Designation)],
    source: TargetSource::Resolved,
};

let eq = m31.position().expect("valid ICRS coordinates");
assert!((eq.ra().degrees() - 10.684_708).abs() < 1e-6);
```

## Configuration

| Type | Field | Default | Effect |
| --- | --- | --- | --- |
| [`ResolverConfig`](https://docs.rs/simbad-resolver/latest/simbad_resolver/struct.ResolverConfig.html) | `online_enabled` | `true` | When `false`, `resolve` only consults the cache. |
| `ResolverConfig` | `namespace` | derived from the seed passed to [`ResolverConfig::new`](https://docs.rs/simbad-resolver/latest/simbad_resolver/struct.ResolverConfig.html#method.new) | UUIDv5 namespace for stable target ids; reuse the same seed across runs. |
| `ResolverConfig` | `fuzzy_min_score` | `None` (disabled) | Minimum token-set similarity for a fuzzy [`SimbadResolver::search`](https://docs.rs/simbad-resolver/latest/simbad_resolver/struct.SimbadResolver.html#method.search) hit; set via [`with_fuzzy`](https://docs.rs/simbad-resolver/latest/simbad_resolver/struct.ResolverConfig.html#method.with_fuzzy). |
| [`SimbadConfig`](https://docs.rs/simbad-resolver/latest/simbad_resolver/struct.SimbadConfig.html) | `endpoint` | SIMBAD TAP sync endpoint | Backend URL for `TapResolver`/`SesameResolver`. |
| `SimbadConfig` | `timeout` | 10s | Per-request timeout; on expiry the resolver returns [`ResolveError::Timeout`](https://docs.rs/simbad-resolver/latest/simbad_resolver/enum.ResolveError.html). |
| `SimbadConfig` | `user_agent` | `simbad-resolver/<version> (+https://github.com/nightwatch-astro/simbad-resolver)` | Identifying header sent with every request (CDS norm). |

## Attribution

This library queries SIMBAD, operated at CDS, Strasbourg, France. Applications
that display resolved data should credit CDS and send an identifying
`User-Agent` header (configurable via `SimbadConfig`).
