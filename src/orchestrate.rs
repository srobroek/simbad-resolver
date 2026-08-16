// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Cache-first single resolution + sticky user override.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use uuid::Uuid;

use crate::cache::redb::Store;
use crate::cache::{Cache, CachedTarget, SearchHit, RANK_FUZZY};
use crate::error::{Error, ResolveError};
use crate::types::{AliasKind, ResolvedAlias, TargetSource};
use crate::{caldwell, config::ResolverConfig, normalize, Resolver};

/// Why a query could not be resolved to a canonical target.
///
/// ```
/// use simbad_resolver::UnresolvedReason;
///
/// // A transient failure and a genuine content miss both leave a query
/// // `Unresolved`, but a caller acts on them differently.
/// assert!(matches!(UnresolvedReason::Offline, UnresolvedReason::Offline));
/// let content_miss = UnresolvedReason::Unknown;
/// assert_ne!(content_miss, UnresolvedReason::Offline);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnresolvedReason {
    /// The online resolver was unreachable/timed out/disabled; the caller may
    /// retry later. Cached/known objects still resolve.
    Offline,
    /// The query is unknown (no such object) or malformed.
    Unknown,
    /// The query maps to several distinct physical objects.
    Ambiguous,
}

/// The outcome of a [`SimbadResolver::resolve`] call.
///
/// ```
/// use simbad_resolver::{Resolution, UnresolvedReason};
///
/// let outcome = Resolution::Unresolved {
///     query: "not-a-real-object".to_owned(),
///     reason: UnresolvedReason::Unknown,
/// };
/// match outcome {
///     Resolution::Resolved(target) => println!("{}", target.primary_designation),
///     Resolution::Unresolved { query, reason } => {
///         println!("{query} did not resolve: {reason:?}");
///     }
/// }
/// ```
#[derive(Clone, Debug, PartialEq)]
pub enum Resolution {
    /// A canonical target (from cache or freshly resolved + cached).
    Resolved(CachedTarget),
    /// No canonical target was found; see [`UnresolvedReason`] for why.
    Unresolved {
        /// The verbatim query.
        query: String,
        /// Why it is unresolved.
        reason: UnresolvedReason,
    },
}

/// How a [`SimbadResolver`] obtains its cache backend.
///
/// The built-in variants select the redb-backed [`Store`](crate::Store) at
/// construction; [`Custom`](Self::Custom) accepts any [`Cache`]. Backend tuning
/// lives on the variant that owns it (e.g. [`FileCache`]), so the constructor
/// signature never grows.
///
/// There is deliberately no "disabled" variant: the cache is load-bearing (the
/// resolve pipeline returns cached rows), so [`InMemory`](Self::InMemory) is the
/// ephemeral, nothing-persisted choice and is what [`Default`] selects. For a
/// truly uncached one-shot lookup, call a bare [`Resolver`] directly instead of
/// the facade.
///
/// ```
/// use simbad_resolver::CacheBackend;
///
/// let ephemeral = CacheBackend::InMemory;
/// let durable = CacheBackend::file("targets.redb"); // opened lazily by `SimbadResolver::new`
/// assert!(matches!(ephemeral, CacheBackend::InMemory));
/// assert!(matches!(durable, CacheBackend::File(_)));
/// ```
pub enum CacheBackend {
    /// Ephemeral in-memory redb store (nothing persisted to disk).
    InMemory,
    /// Durable, file-backed redb store.
    File(FileCache),
    /// A caller-supplied cache backend.
    Custom(Arc<dyn Cache>),
}

impl Default for CacheBackend {
    /// The zero-config default: an ephemeral in-memory store.
    fn default() -> Self {
        Self::InMemory
    }
}

impl CacheBackend {
    /// A file-backed backend at `path` with default options.
    ///
    /// ```
    /// use simbad_resolver::CacheBackend;
    ///
    /// let backend = CacheBackend::file("targets.redb"); // opened lazily by `SimbadResolver::new`
    /// assert!(matches!(backend, CacheBackend::File(_)));
    /// ```
    #[must_use]
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self::File(FileCache::new(path))
    }

    /// Wrap a caller-supplied cache backend.
    ///
    /// ```
    /// use simbad_resolver::{CacheBackend, Store};
    ///
    /// # fn run() -> Result<(), simbad_resolver::CacheError> {
    /// // Any `Cache` impl works, e.g. a `Store`'s own `.cache()`.
    /// let backend = CacheBackend::custom(Store::in_memory()?.cache());
    /// assert!(matches!(backend, CacheBackend::Custom(_)));
    /// # Ok(()) }
    /// ```
    #[must_use]
    pub fn custom(cache: impl Cache + 'static) -> Self {
        Self::Custom(Arc::new(cache))
    }

    /// Materialise the selection into a shared [`Cache`] handle.
    fn into_cache(self) -> Result<Arc<dyn Cache>, Error> {
        match self {
            Self::InMemory => Ok(Arc::new(Store::in_memory()?.cache())),
            Self::File(f) => Ok(Arc::new(Store::open(f.path)?.cache())),
            Self::Custom(c) => Ok(c),
        }
    }
}

/// Options for the built-in file-backed cache.
///
/// Future tunables (cache size, durability, …) are added here, defaulted —
/// never as new constructor arguments.
#[derive(Clone, Debug)]
pub struct FileCache {
    /// Path to the redb database file (created if missing).
    pub path: PathBuf,
}

impl FileCache {
    /// A file cache at `path` with default options.
    ///
    /// ```
    /// use simbad_resolver::FileCache;
    ///
    /// let file_cache = FileCache::new("targets.redb");
    /// assert_eq!(file_cache.path.to_str(), Some("targets.redb"));
    /// ```
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

/// The cache-first resolver facade.
///
/// Generic over any [`Resolver`] backend (TAP, Sesame, offline, fake); the cache
/// backend is chosen at construction via [`CacheBackend`] and type-erased, so
/// the facade type does not carry it.
///
/// This example uses [`crate::OfflineResolver`] and seeds the cache directly, so
/// it runs without a network call — the cache-first lookup below never reaches
/// the resolver.
///
/// ```
/// use simbad_resolver::{
///     AliasKind, CacheBackend, OfflineResolver, ObjectType, Resolution, ResolvedAlias,
///     ResolvedIdentity, ResolverConfig, SimbadResolver, TargetSource,
/// };
///
/// # async fn demo() -> Result<(), simbad_resolver::Error> {
/// let config = ResolverConfig::new("guide.example");
/// let namespace = config.namespace;
/// let facade = SimbadResolver::new(OfflineResolver, CacheBackend::InMemory, config)?;
///
/// let m31 = ResolvedIdentity {
///     simbad_oid: Some(1_575_544),
///     primary_designation: "M 31".to_owned(),
///     common_name: Some("Andromeda Galaxy".to_owned()),
///     object_type: ObjectType::Galaxy,
///     otype_raw: "G".to_owned(),
///     ra_deg: 10.684_708,
///     dec_deg: 41.268_75,
///     v_mag: Some(3.44),
///     galdim_majaxis_arcmin: Some(199.53),
///     galdim_minaxis_arcmin: Some(70.79),
///     galdim_angle_deg: Some(35),
///     aliases: vec![ResolvedAlias::new("M 31", AliasKind::Designation)],
///     source: TargetSource::Seed,
/// };
/// facade.cache().upsert(&m31, &namespace).await?;
///
/// match facade.resolve("M31").await? {
///     Resolution::Resolved(target) => assert_eq!(target.primary_designation, "M 31"),
///     Resolution::Unresolved { .. } => unreachable!("seeded above"),
/// }
/// # Ok(()) }
/// ```
pub struct SimbadResolver<R: Resolver> {
    resolver: R,
    cache: Arc<dyn Cache>,
    config: ResolverConfig,
}

impl<R: Resolver> SimbadResolver<R> {
    /// Construct a facade from a resolver, a [`CacheBackend`], and config.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if a built-in ([`CacheBackend::InMemory`] /
    /// [`CacheBackend::File`]) store cannot be opened or initialised.
    /// [`CacheBackend::Custom`] never fails here.
    ///
    /// ```
    /// use simbad_resolver::{CacheBackend, OfflineResolver, ResolverConfig, SimbadResolver};
    ///
    /// let facade = SimbadResolver::new(
    ///     OfflineResolver,
    ///     CacheBackend::InMemory,
    ///     ResolverConfig::new("my-app.targets"),
    /// )?;
    /// # let _ = facade;
    /// # Ok::<(), simbad_resolver::Error>(())
    /// ```
    pub fn new(resolver: R, cache: CacheBackend, config: ResolverConfig) -> Result<Self, Error> {
        Ok(Self { resolver, cache: cache.into_cache()?, config })
    }

    /// Borrow the underlying cache (e.g. for seeding or enumeration).
    ///
    /// ```
    /// use simbad_resolver::{CacheBackend, OfflineResolver, ResolverConfig, SimbadResolver};
    ///
    /// # async fn run() -> Result<(), simbad_resolver::Error> {
    /// let facade =
    ///     SimbadResolver::new(OfflineResolver, CacheBackend::InMemory, ResolverConfig::new("x"))?;
    /// assert!(facade.cache().list().await?.is_empty(), "freshly constructed cache is empty");
    /// # Ok(()) }
    /// ```
    pub fn cache(&self) -> &dyn Cache {
        self.cache.as_ref()
    }

    /// Borrow the underlying resolver (e.g. to inspect a fake's call count in tests).
    ///
    /// ```
    /// use simbad_resolver::{CacheBackend, OfflineResolver, ResolverConfig, SimbadResolver};
    ///
    /// let facade =
    ///     SimbadResolver::new(OfflineResolver, CacheBackend::InMemory, ResolverConfig::new("x"))?;
    /// let _resolver: &OfflineResolver = facade.resolver();
    /// # Ok::<(), simbad_resolver::Error>(())
    /// ```
    pub fn resolver(&self) -> &R {
        &self.resolver
    }

    /// Local, network-free typeahead search over cached aliases.
    ///
    /// Returns exact/prefix/substring matches (see [`Cache::search`]). When fuzzy
    /// matching is enabled via [`ResolverConfig::with_fuzzy`] and fewer than
    /// `limit` of those are found, the remaining slots are filled with token-set
    /// similarity matches ([`crate::RANK_FUZZY`]), best score first. This fuzzy
    /// matching does not affect [`Self::resolve`], which stays exact-normalized.
    ///
    /// This example seeds the cache directly (no resolver call needed) then
    /// searches it — see [the guide](crate::guide) for the full walkthrough.
    ///
    /// ```
    /// use simbad_resolver::{
    ///     AliasKind, CacheBackend, ObjectType, OfflineResolver, ResolvedAlias, ResolvedIdentity,
    ///     ResolverConfig, SimbadResolver, TargetSource,
    /// };
    ///
    /// # async fn run() -> Result<(), simbad_resolver::Error> {
    /// let config = ResolverConfig::new("example");
    /// let namespace = config.namespace;
    /// let facade = SimbadResolver::new(OfflineResolver, CacheBackend::InMemory, config)?;
    /// let m31 = ResolvedIdentity {
    ///     simbad_oid: Some(1_575_544),
    ///     primary_designation: "M 31".to_owned(),
    ///     common_name: None,
    ///     object_type: ObjectType::Galaxy,
    ///     otype_raw: "G".to_owned(),
    ///     ra_deg: 10.684_708,
    ///     dec_deg: 41.268_75,
    ///     v_mag: Some(3.44),
    ///     galdim_majaxis_arcmin: Some(199.53),
    ///     galdim_minaxis_arcmin: Some(70.79),
    ///     galdim_angle_deg: Some(35),
    ///     aliases: vec![ResolvedAlias::new("M 31", AliasKind::Designation)],
    ///     source: TargetSource::Seed,
    /// };
    /// facade.cache().upsert(&m31, &namespace).await?;
    ///
    /// let hits = facade.search("M 3", 5).await?; // prefix match
    /// assert_eq!(hits[0].target.primary_designation, "M 31");
    /// # Ok(()) }
    /// ```
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, Error> {
        let mut hits = self.cache.search(query, limit).await?;

        if let Some(min_score) = self.config.fuzzy_min_score {
            let q = normalize::normalize(query);
            if !q.is_empty() && hits.len() < limit {
                let already: HashSet<Uuid> = hits.iter().map(|h| h.target.id).collect();

                // Score each not-yet-matched target by its best-matching alias.
                let mut scored: Vec<(f32, usize, SearchHit)> = Vec::new();
                for target in self.cache.list().await? {
                    if already.contains(&target.id) {
                        continue;
                    }
                    let mut best: Option<(f32, String, usize)> = None;
                    for alias in &target.aliases {
                        let score = normalize::jaccard_normalized(&q, &alias.normalized);
                        if score >= min_score
                            && best.as_ref().is_none_or(|(prev, _, _)| score > *prev)
                        {
                            best = Some((score, alias.alias.clone(), alias.normalized.len()));
                        }
                    }
                    if let Some((score, matched_alias, normalized_len)) = best {
                        scored.push((
                            score,
                            normalized_len,
                            SearchHit { target, matched_alias, rank: RANK_FUZZY },
                        ));
                    }
                }

                // Best score first; ties break on the shorter matched alias, then
                // the target's primary designation (stable, deterministic order).
                scored.sort_by(|a, b| {
                    b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then_with(|| {
                        a.2.target.primary_designation.cmp(&b.2.target.primary_designation)
                    })
                });
                for (_, _, hit) in scored.into_iter().take(limit - hits.len()) {
                    hits.push(hit);
                }
            }
        }

        Ok(hits)
    }

    /// Cache-first resolve. On a cache miss and when online is enabled, consult
    /// the resolver, persist the result, and return it; otherwise return a typed
    /// [`Resolution::Unresolved`]. Caldwell designations (`C n`) are translated
    /// to their standard designation first and the original `C n` bound as an
    /// alias.
    ///
    /// See the [struct-level example](Self) for a seeded, network-free
    /// walkthrough, or [`crate::guide`] for the full task-oriented guide.
    ///
    /// ```
    /// use simbad_resolver::{CacheBackend, OfflineResolver, Resolution, ResolverConfig, SimbadResolver};
    ///
    /// # async fn run() -> Result<(), simbad_resolver::Error> {
    /// let facade =
    ///     SimbadResolver::new(OfflineResolver, CacheBackend::InMemory, ResolverConfig::new("x"))?;
    /// // Nothing cached and no network reachable (`OfflineResolver`): unresolved, not an error.
    /// match facade.resolve("M31").await? {
    ///     Resolution::Resolved(_) => unreachable!("cache is empty"),
    ///     Resolution::Unresolved { query, .. } => assert_eq!(query, "M31"),
    /// }
    /// # Ok(()) }
    /// ```
    pub async fn resolve(&self, query: &str) -> Result<Resolution, Error> {
        resolve_core(&self.resolver, self.cache.as_ref(), &self.config, query).await
    }

    /// Bind a chosen canonical target as an authoritative user override, adding
    /// `alias` and making the row sticky (`source = user-override`). Returns the
    /// updated target, or `None` if `target_id` is unknown.
    ///
    /// See [`crate::guide`] for a full "pin a target" walkthrough.
    ///
    /// ```
    /// use simbad_resolver::{
    ///     AliasKind, CacheBackend, ObjectType, OfflineResolver, ResolvedAlias, ResolvedIdentity,
    ///     ResolverConfig, SimbadResolver, TargetSource,
    /// };
    ///
    /// # async fn run() -> Result<(), simbad_resolver::Error> {
    /// let config = ResolverConfig::new("example");
    /// let namespace = config.namespace;
    /// let facade = SimbadResolver::new(OfflineResolver, CacheBackend::InMemory, config)?;
    /// let m31 = ResolvedIdentity {
    ///     simbad_oid: Some(1_575_544),
    ///     primary_designation: "M 31".to_owned(),
    ///     common_name: None,
    ///     object_type: ObjectType::Galaxy,
    ///     otype_raw: "G".to_owned(),
    ///     ra_deg: 10.684_708,
    ///     dec_deg: 41.268_75,
    ///     v_mag: Some(3.44),
    ///     galdim_majaxis_arcmin: Some(199.53),
    ///     galdim_minaxis_arcmin: Some(70.79),
    ///     galdim_angle_deg: Some(35),
    ///     aliases: vec![ResolvedAlias::new("M 31", AliasKind::Designation)],
    ///     source: TargetSource::Seed,
    /// };
    /// let (id, _) = facade.cache().upsert(&m31, &namespace).await?;
    ///
    /// let pinned = facade.apply_override(id, "My Andromeda").await?.expect("id is known");
    /// assert_eq!(pinned.source, TargetSource::UserOverride);
    /// assert!(pinned.aliases.iter().any(|a| a.alias == "My Andromeda"));
    /// # Ok(()) }
    /// ```
    pub async fn apply_override(
        &self,
        target_id: Uuid,
        alias: &str,
    ) -> Result<Option<CachedTarget>, Error> {
        let Some(existing) = self.cache.get_by_id(target_id).await? else {
            return Ok(None);
        };
        let mut identity = existing.to_identity();
        identity.source = TargetSource::UserOverride;
        if !alias.trim().is_empty() && !identity.aliases.iter().any(|a| a.alias == alias) {
            identity.aliases.push(ResolvedAlias::new(alias.to_owned(), AliasKind::User));
        }
        let (id, _) = self.cache.upsert(&identity, &self.config.namespace).await?;
        Ok(self.cache.get_by_id(id).await?)
    }
}

/// The shared cache-first resolve routine used by both the facade and the batch
/// resolver. Caldwell-translates, checks the cache, then (if online) resolves,
/// persists, and re-reads.
pub(crate) async fn resolve_core<R: Resolver, C: Cache + ?Sized>(
    resolver: &R,
    cache: &C,
    config: &ResolverConfig,
    query: &str,
) -> Result<Resolution, Error> {
    // Caldwell translation (facade-level so it applies to any resolver).
    let (simbad_query, caldwell_alias) = match caldwell::parse_caldwell_number(query) {
        Some(n) => match caldwell::caldwell_to_designation(n) {
            Some(desig) => (desig.to_owned(), Some(format!("C {n}"))),
            None => {
                return Ok(Resolution::Unresolved {
                    query: query.to_owned(),
                    reason: UnresolvedReason::Unknown,
                })
            }
        },
        None => (query.to_owned(), None),
    };

    // Cache-first: try the original query, then the translated designation.
    for candidate in [normalize::normalize(query), normalize::normalize(&simbad_query)] {
        if candidate.is_empty() {
            continue;
        }
        if let Some(target) = cache.get_by_normalized(&candidate).await? {
            return Ok(Resolution::Resolved(target));
        }
    }

    if !config.online_enabled {
        return Ok(Resolution::Unresolved {
            query: query.to_owned(),
            reason: UnresolvedReason::Offline,
        });
    }

    match resolver.resolve(&simbad_query).await {
        Ok(mut identity) => {
            if let Some(c) = &caldwell_alias {
                if !identity.aliases.iter().any(|a| &a.alias == c) {
                    identity.aliases.push(ResolvedAlias::new(c.clone(), AliasKind::Designation));
                }
            }
            let (id, _) = cache.upsert(&identity, &config.namespace).await?;
            match cache.get_by_id(id).await? {
                Some(target) => Ok(Resolution::Resolved(target)),
                None => Ok(Resolution::Unresolved {
                    query: query.to_owned(),
                    reason: UnresolvedReason::Unknown,
                }),
            }
        }
        Err(e) => Ok(Resolution::Unresolved { query: query.to_owned(), reason: reason_for(&e) }),
    }
}

/// Map a resolver error to an unresolved reason (transient → Offline).
fn reason_for(e: &ResolveError) -> UnresolvedReason {
    if e.is_transient() {
        UnresolvedReason::Offline
    } else if matches!(e, ResolveError::Ambiguous { .. }) {
        UnresolvedReason::Ambiguous
    } else {
        UnresolvedReason::Unknown
    }
}
