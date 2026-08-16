// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Async batch resolver over a pluggable [`Queue`].
//!
//! Drains pending items cache-first then online, distinguishing **transient**
//! failures (kept pending, retried later, attempt budget preserved) from
//! **content** misses (marked unresolved). Processing is sequential within a
//! drain (polite to the upstream service); each pending item is processed at
//! most once per [`BatchResolver::drain`] call.

use std::collections::HashSet;
use std::sync::Arc;

use crate::cache::{Cache, Queue};
use crate::{
    config::ResolverConfig,
    error::Error,
    orchestrate::{resolve_core, Resolution, UnresolvedReason},
    Resolver,
};

/// Summary of a [`BatchResolver::drain`] pass.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DrainSummary {
    /// Items resolved to a canonical target this pass.
    pub resolved: usize,
    /// Items marked unresolved (content misses) this pass.
    pub unresolved: usize,
    /// Items left pending after a transient failure (to retry later).
    pub still_pending: usize,
}

/// The async batch resolver. Generic over any [`Resolver`]; the cache and queue
/// backends are type-erased so the type does not carry them.
///
/// This example seeds the cache before draining, so [`Self::drain`] resolves
/// `"M31"` from the cache alone — no network call, so it runs without
/// [`crate::OfflineResolver`] ever reaching a resolver backend.
///
/// ```
/// use simbad_resolver::{
///     AliasKind, BatchResolver, Cache, ObjectType, OfflineResolver, ResolvedAlias,
///     ResolvedIdentity, ResolverConfig, Store, TargetSource,
/// };
///
/// # async fn demo() -> Result<(), simbad_resolver::Error> {
/// let store = Store::in_memory()?;
/// let config = ResolverConfig::new("guide.example");
/// let namespace = config.namespace;
///
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
/// store.cache().upsert(&m31, &namespace).await?;
///
/// let batch = BatchResolver::new(OfflineResolver, store.cache(), store.queue(), config)
///     .with_batch_size(8);
/// batch.enqueue("job-1", "M31").await?;
///
/// let summary = batch.drain().await?;
/// assert_eq!(summary.resolved, 1, "cache hit, no resolver call needed");
/// # Ok(()) }
/// ```
pub struct BatchResolver<R: Resolver> {
    resolver: R,
    cache: Arc<dyn Cache>,
    queue: Arc<dyn Queue>,
    config: ResolverConfig,
    batch_size: usize,
}

impl<R: Resolver> BatchResolver<R> {
    /// Construct a batch resolver from caller-supplied cache and queue backends
    /// — typically a [`Store`](crate::Store)'s `.cache()` and `.queue()` over
    /// one shared database.
    ///
    /// ```
    /// use simbad_resolver::{BatchResolver, OfflineResolver, ResolverConfig, Store};
    ///
    /// # fn run() -> Result<(), simbad_resolver::CacheError> {
    /// let store = Store::in_memory()?;
    /// let batch =
    ///     BatchResolver::new(OfflineResolver, store.cache(), store.queue(), ResolverConfig::new("x"));
    /// # let _ = batch;
    /// # Ok(()) }
    /// ```
    pub fn new(
        resolver: R,
        cache: impl Cache + 'static,
        queue: impl Queue + 'static,
        config: ResolverConfig,
    ) -> Self {
        Self { resolver, cache: Arc::new(cache), queue: Arc::new(queue), config, batch_size: 16 }
    }

    /// Set how many pending items are claimed per round (min 1).
    ///
    /// ```
    /// use simbad_resolver::{BatchResolver, OfflineResolver, ResolverConfig, Store};
    ///
    /// # fn run() -> Result<(), simbad_resolver::CacheError> {
    /// let store = Store::in_memory()?;
    /// let batch =
    ///     BatchResolver::new(OfflineResolver, store.cache(), store.queue(), ResolverConfig::new("x"))
    ///         .with_batch_size(0); // clamped to 1
    /// # let _ = batch;
    /// # Ok(()) }
    /// ```
    #[must_use]
    pub fn with_batch_size(mut self, n: usize) -> Self {
        self.batch_size = n.max(1);
        self
    }

    /// Borrow the underlying queue.
    ///
    /// ```
    /// use simbad_resolver::{BatchResolver, OfflineResolver, ResolverConfig, Store};
    ///
    /// # async fn run() -> Result<(), simbad_resolver::Error> {
    /// let store = Store::in_memory()?;
    /// let batch =
    ///     BatchResolver::new(OfflineResolver, store.cache(), store.queue(), ResolverConfig::new("x"));
    /// assert_eq!(batch.queue().pending_count().await?, 0);
    /// # Ok(()) }
    /// ```
    pub fn queue(&self) -> &dyn Queue {
        self.queue.as_ref()
    }

    /// Enqueue an identifier keyed by an opaque caller id (idempotent).
    ///
    /// ```
    /// use simbad_resolver::{BatchResolver, OfflineResolver, ResolverConfig, Store};
    ///
    /// # async fn run() -> Result<(), simbad_resolver::Error> {
    /// let store = Store::in_memory()?;
    /// let batch =
    ///     BatchResolver::new(OfflineResolver, store.cache(), store.queue(), ResolverConfig::new("x"));
    /// batch.enqueue("job-1", "M31").await?;
    /// batch.enqueue("job-1", "M31").await?; // idempotent: still one pending item
    /// assert_eq!(batch.queue().pending_count().await?, 1);
    /// # Ok(()) }
    /// ```
    pub async fn enqueue(&self, id: &str, query: &str) -> Result<(), Error> {
        self.queue.enqueue(id, query).await?;
        Ok(())
    }

    /// Drain the pending queue once: resolve each pending item cache-first then
    /// online, marking resolved/unresolved or leaving it pending on a transient
    /// failure. Returns a [`DrainSummary`]. Terminates once every currently
    /// pending item has been processed (transiently-failed items are retried on
    /// a subsequent call).
    ///
    /// See the [struct-level example](Self) for a full seed-then-drain walkthrough.
    ///
    /// ```
    /// use simbad_resolver::{BatchResolver, OfflineResolver, ResolverConfig, Store};
    ///
    /// # async fn run() -> Result<(), simbad_resolver::Error> {
    /// let store = Store::in_memory()?;
    /// let batch =
    ///     BatchResolver::new(OfflineResolver, store.cache(), store.queue(), ResolverConfig::new("x"));
    /// batch.enqueue("job-1", "does-not-exist").await?;
    ///
    /// // OfflineResolver reports `Disabled` (transient), so the item stays pending.
    /// let summary = batch.drain().await?;
    /// assert_eq!(summary.still_pending, 1);
    /// # Ok(()) }
    /// ```
    pub async fn drain(&self) -> Result<DrainSummary, Error> {
        let mut summary = DrainSummary::default();
        let mut seen: HashSet<String> = HashSet::new();
        loop {
            let batch = self.queue.claim_pending(self.batch_size).await?;
            let fresh: Vec<_> = batch.into_iter().filter(|it| !seen.contains(&it.id)).collect();
            if fresh.is_empty() {
                break;
            }
            for item in fresh {
                seen.insert(item.id.clone());
                match resolve_core(&self.resolver, self.cache.as_ref(), &self.config, &item.query)
                    .await?
                {
                    Resolution::Resolved(target) => {
                        self.queue.mark_resolved(&item.id, target.id).await?;
                        summary.resolved += 1;
                    }
                    Resolution::Unresolved { reason: UnresolvedReason::Offline, .. } => {
                        self.queue.release(&item.id).await?;
                        summary.still_pending += 1;
                    }
                    Resolution::Unresolved { .. } => {
                        self.queue.mark_unresolved(&item.id).await?;
                        summary.unresolved += 1;
                    }
                }
            }
        }
        Ok(summary)
    }
}
