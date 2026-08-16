// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The network-resolution seam: [`Resolver`], [`PositionResolver`],
//! [`OfflineResolver`], and (test-only) [`FakeResolver`].
//!
//! Object-safe via `async-trait` so a boxed `dyn Resolver` can be shared
//! across an async runtime.

use crate::error::ResolveError;
use crate::types::{PositionMatch, ResolvedIdentity};

/// Resolve a designation or common name to one canonical identity.
///
/// Zero rows resolve to [`ResolveError::NotFound`], more than one distinct
/// physical object to [`ResolveError::Ambiguous`], a transport/timeout failure
/// to [`ResolveError::Network`]/[`ResolveError::Timeout`], a disabled backend
/// to [`ResolveError::Disabled`], and a malformed response to
/// [`ResolveError::Parse`] — implementations MUST return one of these variants
/// rather than a synthesized identity.
///
/// Implementations MUST be `Send + Sync` so a resolver can be shared across a
/// background queue.
#[async_trait::async_trait]
pub trait Resolver: Send + Sync {
    /// Resolve a complete designation or common name to a canonical identity.
    ///
    /// Any [`crate::TapResolver`]/[`crate::SesameResolver`] call needs network
    /// access (see their own docs for `no_run` examples); this one uses the
    /// offline [`crate::FakeResolver`] test double instead:
    ///
    /// ```
    /// use simbad_resolver::{FakeResolver, ResolveError, Resolver};
    ///
    /// # async fn run() {
    /// let resolver = FakeResolver::new(); // no canned responses registered
    /// let err = resolver.resolve("does-not-exist").await.unwrap_err();
    /// assert!(matches!(err, ResolveError::NotFound(_)));
    /// # }
    /// ```
    async fn resolve(&self, query: &str) -> Result<ResolvedIdentity, ResolveError>;

    /// Resolve a verbatim FITS `OBJECT` header value to a canonical identity.
    ///
    /// Matching is exact-normalized only (no fuzzy/probabilistic match); a
    /// non-matching or ambiguous value stays unresolved rather than being
    /// guessed. The default delegates to [`Resolver::resolve`];
    /// implementations only override this when they need FITS-specific
    /// pre-normalization.
    ///
    /// ```
    /// use simbad_resolver::{
    ///     AliasKind, FakeResolver, ObjectType, ResolvedAlias, ResolvedIdentity, Resolver,
    ///     TargetSource,
    /// };
    ///
    /// # async fn run() {
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
    /// let resolver = FakeResolver::new().with_response("M31", m31);
    /// // The default impl just forwards to `resolve`.
    /// let got = resolver.resolve_object("M 31").await.unwrap();
    /// assert_eq!(got.primary_designation, "M 31");
    /// # }
    /// ```
    async fn resolve_object(&self, object_raw: &str) -> Result<ResolvedIdentity, ResolveError> {
        self.resolve(object_raw).await
    }
}

/// Position resolution (cone search) is a separate capability some resolver
/// backends provide; not every [`Resolver`] supports it.
///
/// [`crate::TapResolver`] is the built-in implementation; it queries the real
/// SIMBAD TAP endpoint, so this example is `no_run`:
///
/// ```no_run
/// use simbad_resolver::{PositionResolver, TapResolver};
///
/// # async fn run() -> Result<(), simbad_resolver::ResolveError> {
/// let resolver = TapResolver::with_defaults()?;
/// // Objects within 0.05° of M 31's ICRS position, nearest first (max 5).
/// let matches = resolver.resolve_position(10.684_708, 41.268_75, 0.05, 5).await?;
/// for m in &matches {
///     println!("{} at {:.4}°", m.identity.primary_designation, m.separation_deg);
/// }
/// # Ok(())
/// # }
/// ```
#[async_trait::async_trait]
pub trait PositionResolver: Send + Sync {
    /// Nearest object(s) within `radius_deg` of `(ra_deg, dec_deg)`, nearest first.
    async fn resolve_position(
        &self,
        ra_deg: f64,
        dec_deg: f64,
        radius_deg: f64,
        limit: usize,
    ) -> Result<Vec<PositionMatch>, ResolveError>;
}

/// A zero-cost [`Resolver`] that never reaches the network.
///
/// Used when online resolution is disabled by configuration, so callers can
/// run a cache-first use case without constructing a network client. Every
/// call reports [`ResolveError::Disabled`].
///
/// ```
/// use simbad_resolver::{OfflineResolver, ResolveError, Resolver};
///
/// # async fn run() {
/// let err = OfflineResolver.resolve("M 31").await.unwrap_err();
/// assert_eq!(err, ResolveError::Disabled);
/// # }
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct OfflineResolver;

#[async_trait::async_trait]
impl Resolver for OfflineResolver {
    async fn resolve(&self, _query: &str) -> Result<ResolvedIdentity, ResolveError> {
        Err(ResolveError::Disabled)
    }
}

// ── FakeResolver (test double) ────────────────────────────────────────────────

/// In-memory test double for [`Resolver`].
///
/// Returns canned results without any network access. Gated behind
/// `cfg(test)` and the `test-util` feature so it is available to downstream
/// tests but never compiled into production builds.
///
/// Lookups are keyed by the *normalized* query ([`crate::normalize::normalize`])
/// so callers can register a canned identity under any of its aliases.
///
/// The `call_count` atomic counter increments on every call to
/// [`Resolver::resolve`]; read it with [`FakeResolver::call_count`] to assert
/// the resolver was invoked an exact number of times (useful for verifying
/// cache-first behaviour).
#[cfg(any(test, feature = "test-util"))]
#[derive(Debug, Default)]
pub struct FakeResolver {
    /// Normalized query → canned successful identity.
    responses: std::collections::HashMap<String, ResolvedIdentity>,
    /// Normalized query → canned error (takes precedence over `responses`).
    errors: std::collections::HashMap<String, ResolveError>,
    /// Error returned for any query with no registered response/error.
    default_error: Option<ResolveError>,
    /// Number of times [`Resolver::resolve`] has been called.
    call_count: std::sync::atomic::AtomicUsize,
}

#[cfg(any(test, feature = "test-util"))]
impl FakeResolver {
    /// Construct an empty fake; unknown queries return [`ResolveError::NotFound`].
    ///
    /// ```
    /// use simbad_resolver::{FakeResolver, ResolveError, Resolver};
    ///
    /// # async fn run() {
    /// let err = FakeResolver::new().resolve("anything").await.unwrap_err();
    /// assert!(matches!(err, ResolveError::NotFound(_)));
    /// # }
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a canned successful identity, keyed by the normalized `query`.
    ///
    /// ```
    /// use simbad_resolver::{
    ///     AliasKind, FakeResolver, ObjectType, ResolvedAlias, ResolvedIdentity, Resolver,
    ///     TargetSource,
    /// };
    ///
    /// # async fn run() {
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
    /// let resolver = FakeResolver::new().with_response("M31", m31);
    /// // Registered under normalized "M31"; any equivalent form matches.
    /// let got = resolver.resolve("M 31").await.unwrap();
    /// assert_eq!(got.primary_designation, "M 31");
    /// # }
    /// ```
    #[must_use]
    pub fn with_response(mut self, query: &str, identity: ResolvedIdentity) -> Self {
        self.responses.insert(crate::normalize::normalize(query), identity);
        self
    }

    /// Register a canned error for `query`, keyed by its normalized form.
    ///
    /// ```
    /// use simbad_resolver::{FakeResolver, ResolveError, Resolver};
    ///
    /// # async fn run() {
    /// let resolver = FakeResolver::new()
    ///     .with_error("M31", ResolveError::Ambiguous { query: "M31".to_owned(), count: 2 });
    /// let err = resolver.resolve("M 31").await.unwrap_err();
    /// assert!(matches!(err, ResolveError::Ambiguous { count: 2, .. }));
    /// # }
    /// ```
    #[must_use]
    pub fn with_error(mut self, query: &str, error: ResolveError) -> Self {
        self.errors.insert(crate::normalize::normalize(query), error);
        self
    }

    /// Set the error returned for any unregistered query (default:
    /// [`ResolveError::NotFound`]). Use [`ResolveError::Network`] to simulate
    /// an offline resolver for degrade-to-cache tests.
    ///
    /// ```
    /// use simbad_resolver::{FakeResolver, ResolveError, Resolver};
    ///
    /// # async fn run() {
    /// let resolver =
    ///     FakeResolver::new().with_default_error(ResolveError::Network("down".to_owned()));
    /// let err = resolver.resolve("anything").await.unwrap_err();
    /// assert!(matches!(err, ResolveError::Network(_)));
    /// # }
    /// ```
    #[must_use]
    pub fn with_default_error(mut self, error: ResolveError) -> Self {
        self.default_error = Some(error);
        self
    }

    /// Return the number of times [`Resolver::resolve`] has been called.
    ///
    /// Uses `Relaxed` ordering; suitable for single-threaded test assertions
    /// after all async work has completed.
    ///
    /// ```
    /// use simbad_resolver::{FakeResolver, Resolver};
    ///
    /// # async fn run() {
    /// let resolver = FakeResolver::new();
    /// assert_eq!(resolver.call_count(), 0);
    /// let _ = resolver.resolve("M 31").await;
    /// assert_eq!(resolver.call_count(), 1);
    /// # }
    /// ```
    #[must_use]
    pub fn call_count(&self) -> usize {
        self.call_count.load(std::sync::atomic::Ordering::Relaxed)
    }
}

// `FakeResolver` cannot derive `Clone` because `AtomicUsize` does not
// implement `Clone`. Provide a manual impl that resets the counter in the
// clone so each clone starts from zero.
#[cfg(any(test, feature = "test-util"))]
impl Clone for FakeResolver {
    fn clone(&self) -> Self {
        Self {
            responses: self.responses.clone(),
            errors: self.errors.clone(),
            default_error: self.default_error.clone(),
            call_count: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[cfg(any(test, feature = "test-util"))]
#[async_trait::async_trait]
impl Resolver for FakeResolver {
    async fn resolve(&self, query: &str) -> Result<ResolvedIdentity, ResolveError> {
        self.call_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let key = crate::normalize::normalize(query);
        if let Some(err) = self.errors.get(&key) {
            return Err(err.clone());
        }
        if let Some(identity) = self.responses.get(&key) {
            return Ok(identity.clone());
        }
        Err(self.default_error.clone().unwrap_or_else(|| ResolveError::NotFound(query.to_owned())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AliasKind, ObjectType, ResolvedAlias, TargetSource};

    fn sample_identity() -> ResolvedIdentity {
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

    #[tokio::test]
    async fn fake_resolver_returns_canned_response() {
        let resolver = FakeResolver::new().with_response("M31", sample_identity());
        // Registered under normalized "M31"; querying any alias form hits it.
        let got = resolver.resolve("M 31").await.unwrap();
        assert_eq!(got.primary_designation, "M 31");
        assert_eq!(got.object_type, ObjectType::Galaxy);
        assert_eq!(got.otype_raw, "G");
        assert_eq!(got.simbad_oid, Some(1_575_544));
    }

    #[tokio::test]
    async fn fake_resolver_unknown_is_not_found() {
        let resolver = FakeResolver::new();
        let err = resolver.resolve("does-not-exist").await.unwrap_err();
        assert!(matches!(err, ResolveError::NotFound(_)));
    }

    #[tokio::test]
    async fn fake_resolver_canned_error() {
        let resolver = FakeResolver::new()
            .with_error("M31", ResolveError::Ambiguous { query: "M31".to_owned(), count: 2 });
        let err = resolver.resolve("M 31").await.unwrap_err();
        assert!(matches!(err, ResolveError::Ambiguous { count: 2, .. }));
    }

    #[tokio::test]
    async fn fake_resolver_default_error_simulates_offline() {
        let resolver =
            FakeResolver::new().with_default_error(ResolveError::Network("down".to_owned()));
        let err = resolver.resolve("anything").await.unwrap_err();
        assert!(matches!(err, ResolveError::Network(_)));
    }

    #[tokio::test]
    async fn resolve_object_defaults_to_resolve() {
        let resolver = FakeResolver::new().with_response("M31", sample_identity());
        let got = resolver.resolve_object("M 31").await.unwrap();
        assert_eq!(got.primary_designation, "M 31");
    }

    #[tokio::test]
    async fn fake_resolver_call_count_increments() {
        let resolver = FakeResolver::new().with_response("M31", sample_identity());
        assert_eq!(resolver.call_count(), 0);
        let _ = resolver.resolve("M 31").await;
        let _ = resolver.resolve("m31").await;
        assert_eq!(resolver.call_count(), 2);
    }

    #[tokio::test]
    async fn fake_resolver_clone_resets_call_counter() {
        let resolver = FakeResolver::new().with_response("M31", sample_identity());
        let _ = resolver.resolve("M 31").await;
        assert_eq!(resolver.call_count(), 1);

        // A clone copies the canned responses but starts its own counter at 0.
        let cloned = resolver.clone();
        assert_eq!(cloned.call_count(), 0);
        let got = cloned.resolve("M 31").await.unwrap();
        assert_eq!(got.primary_designation, "M 31");
        assert_eq!(cloned.call_count(), 1);
        assert_eq!(resolver.call_count(), 1, "clone's calls do not affect the original");
    }

    #[tokio::test]
    async fn offline_resolver_always_disabled() {
        let resolver = OfflineResolver;
        let err = resolver.resolve("M 31").await.unwrap_err();
        assert_eq!(err, ResolveError::Disabled);
    }

    /// A minimal [`PositionResolver`] impl, used only to prove the trait is
    /// object-safe (`Box<dyn PositionResolver>`) and callable.
    struct FakePositionResolver;

    #[async_trait::async_trait]
    impl PositionResolver for FakePositionResolver {
        async fn resolve_position(
            &self,
            _ra_deg: f64,
            _dec_deg: f64,
            _radius_deg: f64,
            limit: usize,
        ) -> Result<Vec<PositionMatch>, ResolveError> {
            let identity = sample_identity();
            let matches: Vec<PositionMatch> =
                std::iter::once(PositionMatch { identity, separation_deg: 0.01 })
                    .take(limit)
                    .collect();
            Ok(matches)
        }
    }

    #[tokio::test]
    async fn position_resolver_is_object_safe_and_callable() {
        let resolver: Box<dyn PositionResolver> = Box::new(FakePositionResolver);
        let matches = resolver.resolve_position(10.68, 41.27, 0.5, 5).await.unwrap();
        assert_eq!(matches.len(), 1);
        assert!((matches[0].separation_deg - 0.01).abs() < 1e-9);
    }
}
