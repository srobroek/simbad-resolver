// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Pluggable `Cache` and `Queue` traits and their read models.
//!
//! The [`Cache`] is a durable dedup/typeahead store of canonical target
//! identities (not a TTL/eviction cache). The [`Queue`] is the pending-work
//! store the async batch resolver drains. Both are `async` and object-safe;
//! the built-in [`redb`] backend ([`redb::Store`]) serves both durable
//! (file-backed) and ephemeral (in-memory) modes, and callers may supply their
//! own.

pub mod redb;

use uuid::Uuid;

use crate::types::{ObjectType, ResolvedAlias, ResolvedIdentity, TargetSource};

// ── Read models ──────────────────────────────────────────────────────────────

/// A cached canonical target plus its aliases, as read back from a [`Cache`].
///
/// Mirrors [`ResolvedIdentity`] but additionally carries the persisted
/// [`CachedTarget::id`] and [`CachedTarget::resolved_at`].
#[derive(Clone, Debug, PartialEq)]
pub struct CachedTarget {
    /// Persisted target id (UUIDv5 derived from the caller namespace).
    pub id: Uuid,
    /// SIMBAD physical-object id (dedup key) when resolved online.
    pub simbad_oid: Option<i64>,
    /// Canonical display designation.
    pub primary_designation: String,
    /// Curated common name when one exists.
    pub common_name: Option<String>,
    /// Closed object-type classification.
    pub object_type: ObjectType,
    /// Raw SIMBAD `otype` string (escape hatch alongside `object_type`).
    pub otype_raw: String,
    /// ICRS J2000 right ascension, decimal degrees.
    pub ra_deg: f64,
    /// ICRS J2000 declination, decimal degrees.
    pub dec_deg: f64,
    /// Johnson V-band apparent magnitude (SIMBAD `allfluxes.V`) when known.
    pub v_mag: Option<f64>,
    /// Angular major-axis diameter in arcminutes, unconverted; `None` for a
    /// point source.
    pub galdim_majaxis_arcmin: Option<f64>,
    /// Angular minor-axis diameter in arcminutes, unconverted; `None` under the
    /// same condition as [`Self::galdim_majaxis_arcmin`].
    pub galdim_minaxis_arcmin: Option<f64>,
    /// Position angle of the major axis in degrees, unconverted; `None` under
    /// the same condition as [`Self::galdim_majaxis_arcmin`].
    pub galdim_angle_deg: Option<i16>,
    /// Provenance of the stored identity.
    pub source: TargetSource,
    /// RFC 3339 timestamp of the last seed/resolve/override.
    pub resolved_at: String,
    /// All aliases (designations + common names + user-added).
    pub aliases: Vec<ResolvedAlias>,
}

impl CachedTarget {
    /// Build a [`ResolvedIdentity`] view of this cached target.
    ///
    /// ```
    /// use simbad_resolver::{AliasKind, CachedTarget, ObjectType, ResolvedAlias, TargetSource};
    /// use uuid::Uuid;
    ///
    /// let target = CachedTarget {
    ///     id: Uuid::nil(),
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
    ///     source: TargetSource::Resolved,
    ///     resolved_at: "2026-07-11T00:00:00Z".to_owned(),
    ///     aliases: vec![ResolvedAlias::new("NGC 224", AliasKind::Designation)],
    /// };
    /// let identity = target.to_identity();
    /// assert_eq!(identity.primary_designation, "M 31");
    /// assert_eq!(identity.aliases, target.aliases);
    /// ```
    #[must_use]
    pub fn to_identity(&self) -> ResolvedIdentity {
        ResolvedIdentity {
            simbad_oid: self.simbad_oid,
            primary_designation: self.primary_designation.clone(),
            common_name: self.common_name.clone(),
            object_type: self.object_type,
            otype_raw: self.otype_raw.clone(),
            ra_deg: self.ra_deg,
            dec_deg: self.dec_deg,
            v_mag: self.v_mag,
            galdim_majaxis_arcmin: self.galdim_majaxis_arcmin,
            galdim_minaxis_arcmin: self.galdim_minaxis_arcmin,
            galdim_angle_deg: self.galdim_angle_deg,
            aliases: self.aliases.clone(),
            source: self.source,
        }
    }

    /// This target's sky position as a typed `skymath::Equatorial` coordinate.
    ///
    /// SIMBAD coordinates are ICRS; at planning grade (≤ ~1 arcminute) ICRS is
    /// treated as J2000. The raw [`Self::ra_deg`] / [`Self::dec_deg`] fields
    /// remain the canonical storage.
    ///
    /// # Errors
    ///
    /// `skymath::Error::OutOfRange` if the stored values are outside RA
    /// `[0, 360)` / Dec `[-90, +90]` (malformed cache content).
    ///
    /// ```
    /// use simbad_resolver::{AliasKind, CachedTarget, ObjectType, ResolvedAlias, TargetSource};
    /// use uuid::Uuid;
    ///
    /// # fn run() -> Result<(), skymath::Error> {
    /// let target = CachedTarget {
    ///     id: Uuid::nil(), simbad_oid: Some(1_575_544), primary_designation: "M 31".to_owned(),
    ///     common_name: None, object_type: ObjectType::Galaxy, otype_raw: "G".to_owned(),
    ///     ra_deg: 10.684_708, dec_deg: 41.268_75, v_mag: Some(3.44),
    ///     galdim_majaxis_arcmin: None, galdim_minaxis_arcmin: None, galdim_angle_deg: None,
    ///     source: TargetSource::Resolved, resolved_at: "2026-07-11T00:00:00Z".to_owned(),
    ///     aliases: vec![ResolvedAlias::new("M 31", AliasKind::Designation)],
    /// };
    /// let eq = target.position()?;
    /// assert!((eq.ra().degrees() - 10.684_708).abs() < 1e-6);
    /// # Ok(()) }
    /// ```
    pub fn position(&self) -> skymath::Result<skymath::Equatorial> {
        skymath::Equatorial::j2000(
            skymath::Angle::from_degrees(self.ra_deg),
            skymath::Angle::from_degrees(self.dec_deg),
        )
    }
}

/// A single ranked typeahead hit.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit {
    /// The matched canonical target (aliases loaded).
    pub target: CachedTarget,
    /// The display form of the alias that matched.
    pub matched_alias: String,
    /// Rank bucket: `0` exact, `1` prefix, `2` substring, `3` fuzzy.
    pub rank: u8,
}

/// Rank bucket for an exact normalized-alias match.
pub const RANK_EXACT: u8 = 0;
/// Rank bucket for a prefix match.
pub const RANK_PREFIX: u8 = 1;
/// Rank bucket for a substring match.
pub const RANK_SUBSTRING: u8 = 2;
/// Rank bucket for a fuzzy (token-set similarity) match.
///
/// Only produced by the facade [`crate::SimbadResolver::search`] when fuzzy
/// matching is enabled via [`crate::ResolverConfig::with_fuzzy`]; the
/// [`Cache::search`] trait method itself never returns this rank.
pub const RANK_FUZZY: u8 = 3;

/// Outcome of a [`Cache::upsert`] call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpsertOutcome {
    /// A new row was inserted.
    Inserted,
    /// An existing row (matched by oid or derived id) was updated.
    Updated,
    /// Skipped: an existing `user-override` row takes precedence.
    SkippedUserOverride,
}

/// State of a queued batch item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PendingState {
    /// Awaiting resolution (or a retry after a transient failure).
    Pending,
    /// Resolved to a canonical target.
    Resolved,
    /// A genuine content miss (unknown/ambiguous).
    Unresolved,
}

impl PendingState {
    /// The wire/DB string.
    ///
    /// ```
    /// use simbad_resolver::PendingState;
    ///
    /// assert_eq!(PendingState::Unresolved.as_wire(), "unresolved");
    /// ```
    #[must_use]
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Resolved => "resolved",
            Self::Unresolved => "unresolved",
        }
    }

    /// Parse a wire/DB string; unknown → `None`.
    ///
    /// ```
    /// use simbad_resolver::PendingState;
    ///
    /// assert_eq!(PendingState::from_wire("resolved"), Some(PendingState::Resolved));
    /// assert_eq!(PendingState::from_wire("bogus"), None);
    /// ```
    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "resolved" => Some(Self::Resolved),
            "unresolved" => Some(Self::Unresolved),
            _ => None,
        }
    }
}

/// One queued batch item (read model for a [`Queue`]).
#[derive(Clone, Debug, PartialEq)]
pub struct PendingItem {
    /// Opaque caller id (the queue key).
    pub id: String,
    /// Raw identifier to resolve.
    pub query: String,
    /// Current state.
    pub state: PendingState,
    /// Attempt counter (incremented only on content misses).
    pub attempts: i64,
    /// Resolved canonical-target id, when resolved.
    pub target_id: Option<Uuid>,
}

// ── Errors ───────────────────────────────────────────────────────────────────

/// Error type for [`Cache`] operations.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    /// Underlying backend failure (DB error, etc.).
    #[error("cache backend error: {0}")]
    Backend(String),
    /// A stored id was not a valid UUID.
    #[error("invalid stored uuid '{0}': {1}")]
    InvalidUuid(String, uuid::Error),
    /// A stored enum value was outside its closed set.
    #[error("invalid stored enum value: '{0}'")]
    InvalidEnum(String),
}

/// Error type for [`Queue`] operations.
#[derive(Debug, thiserror::Error)]
pub enum QueueError {
    /// Underlying backend failure.
    #[error("queue backend error: {0}")]
    Backend(String),
    /// A stored id was not a valid UUID.
    #[error("invalid stored uuid '{0}': {1}")]
    InvalidUuid(String, uuid::Error),
    /// A stored state value was outside its closed set.
    #[error("invalid stored state: '{0}'")]
    InvalidState(String),
}

// ── Cache trait ──────────────────────────────────────────────────────────────

/// The pluggable identity store.
///
/// Implementations MUST honour dedup + source precedence in [`Cache::upsert`]:
/// match an existing row by `simbad_oid` when `Some`, else by the caller's
/// designation-derived id ([`crate::identity::target_id_from_designation`]); a
/// write proceeds iff `incoming.source.may_overwrite(existing.source)` (a
/// `user-override` row is sticky). Aliases are rewritten wholesale on update.
#[async_trait::async_trait]
pub trait Cache: Send + Sync {
    /// Read a cached target by its persisted id.
    ///
    /// These trait-method examples all use the built-in [`crate::Store`]'s
    /// in-memory [`Cache`] (`RedbCache`) — local redb access, no network.
    ///
    /// ```
    /// use simbad_resolver::{AliasKind, Cache, ObjectType, ResolvedAlias, ResolvedIdentity, Store, TargetSource};
    ///
    /// # async fn run() -> Result<(), simbad_resolver::CacheError> {
    /// let store = Store::in_memory()?;
    /// let m31 = ResolvedIdentity {
    ///     simbad_oid: Some(1_575_544), primary_designation: "M 31".to_owned(),
    ///     common_name: None, object_type: ObjectType::Galaxy, otype_raw: "G".to_owned(),
    ///     ra_deg: 10.684_708, dec_deg: 41.268_75, v_mag: Some(3.44),
    ///     galdim_majaxis_arcmin: None, galdim_minaxis_arcmin: None, galdim_angle_deg: None,
    ///     aliases: vec![ResolvedAlias::new("M 31", AliasKind::Designation)],
    ///     source: TargetSource::Seed,
    /// };
    /// let (id, _) = store.cache().upsert(&m31, &uuid::Uuid::nil()).await?;
    /// let target = store.cache().get_by_id(id).await?.expect("just inserted");
    /// assert_eq!(target.primary_designation, "M 31");
    /// # Ok(()) }
    /// ```
    async fn get_by_id(&self, id: Uuid) -> Result<Option<CachedTarget>, CacheError>;

    /// Read a cached target by its SIMBAD physical-object id.
    ///
    /// ```
    /// use simbad_resolver::{AliasKind, Cache, ObjectType, ResolvedAlias, ResolvedIdentity, Store, TargetSource};
    ///
    /// # async fn run() -> Result<(), simbad_resolver::CacheError> {
    /// let store = Store::in_memory()?;
    /// let m31 = ResolvedIdentity {
    ///     simbad_oid: Some(1_575_544), primary_designation: "M 31".to_owned(),
    ///     common_name: None, object_type: ObjectType::Galaxy, otype_raw: "G".to_owned(),
    ///     ra_deg: 10.684_708, dec_deg: 41.268_75, v_mag: Some(3.44),
    ///     galdim_majaxis_arcmin: None, galdim_minaxis_arcmin: None, galdim_angle_deg: None,
    ///     aliases: vec![ResolvedAlias::new("M 31", AliasKind::Designation)],
    ///     source: TargetSource::Seed,
    /// };
    /// store.cache().upsert(&m31, &uuid::Uuid::nil()).await?;
    /// let target = store.cache().get_by_simbad_oid(1_575_544).await?.expect("dedup key matches");
    /// assert_eq!(target.primary_designation, "M 31");
    /// # Ok(()) }
    /// ```
    async fn get_by_simbad_oid(&self, oid: i64) -> Result<Option<CachedTarget>, CacheError>;

    /// Read a cached target by an exact normalized alias (normalize the query first).
    ///
    /// ```
    /// use simbad_resolver::{
    ///     normalize::normalize, AliasKind, Cache, ObjectType, ResolvedAlias, ResolvedIdentity,
    ///     Store, TargetSource,
    /// };
    ///
    /// # async fn run() -> Result<(), simbad_resolver::CacheError> {
    /// let store = Store::in_memory()?;
    /// let m31 = ResolvedIdentity {
    ///     simbad_oid: Some(1_575_544), primary_designation: "M 31".to_owned(),
    ///     common_name: None, object_type: ObjectType::Galaxy, otype_raw: "G".to_owned(),
    ///     ra_deg: 10.684_708, dec_deg: 41.268_75, v_mag: Some(3.44),
    ///     galdim_majaxis_arcmin: None, galdim_minaxis_arcmin: None, galdim_angle_deg: None,
    ///     aliases: vec![ResolvedAlias::new("M 31", AliasKind::Designation)],
    ///     source: TargetSource::Seed,
    /// };
    /// store.cache().upsert(&m31, &uuid::Uuid::nil()).await?;
    /// let target = store.cache().get_by_normalized(&normalize("M31")).await?.expect("normalized match");
    /// assert_eq!(target.primary_designation, "M 31");
    /// # Ok(()) }
    /// ```
    async fn get_by_normalized(&self, normalized: &str)
        -> Result<Option<CachedTarget>, CacheError>;

    /// Ranked typeahead search over aliases: exact > prefix > substring, deduped
    /// to one hit per target (best rank wins, ties → shortest alias), capped to
    /// `limit`. Local-only, no network. A blank query or `limit == 0` → empty.
    ///
    /// See [`crate::SimbadResolver::search`] for the facade wrapper that also
    /// adds a fuzzy tier on top of this.
    ///
    /// ```
    /// use simbad_resolver::{AliasKind, Cache, ObjectType, ResolvedAlias, ResolvedIdentity, Store, TargetSource};
    ///
    /// # async fn run() -> Result<(), simbad_resolver::CacheError> {
    /// let store = Store::in_memory()?;
    /// let m31 = ResolvedIdentity {
    ///     simbad_oid: Some(1_575_544), primary_designation: "M 31".to_owned(),
    ///     common_name: None, object_type: ObjectType::Galaxy, otype_raw: "G".to_owned(),
    ///     ra_deg: 10.684_708, dec_deg: 41.268_75, v_mag: Some(3.44),
    ///     galdim_majaxis_arcmin: None, galdim_minaxis_arcmin: None, galdim_angle_deg: None,
    ///     aliases: vec![ResolvedAlias::new("M 31", AliasKind::Designation)],
    ///     source: TargetSource::Seed,
    /// };
    /// store.cache().upsert(&m31, &uuid::Uuid::nil()).await?;
    /// let hits = store.cache().search("M 3", 5).await?; // prefix match
    /// assert_eq!(hits[0].target.primary_designation, "M 31");
    /// # Ok(()) }
    /// ```
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, CacheError>;

    /// Upsert an identity (and its aliases) with dedup + precedence. `namespace`
    /// is the caller's id namespace for the designation-derived fallback id.
    ///
    /// ```
    /// use simbad_resolver::{
    ///     AliasKind, Cache, ObjectType, ResolvedAlias, ResolvedIdentity, Store, TargetSource,
    ///     UpsertOutcome,
    /// };
    ///
    /// # async fn run() -> Result<(), simbad_resolver::CacheError> {
    /// let store = Store::in_memory()?;
    /// let m31 = ResolvedIdentity {
    ///     simbad_oid: Some(1_575_544), primary_designation: "M 31".to_owned(),
    ///     common_name: None, object_type: ObjectType::Galaxy, otype_raw: "G".to_owned(),
    ///     ra_deg: 10.684_708, dec_deg: 41.268_75, v_mag: Some(3.44),
    ///     galdim_majaxis_arcmin: None, galdim_minaxis_arcmin: None, galdim_angle_deg: None,
    ///     aliases: vec![ResolvedAlias::new("M 31", AliasKind::Designation)],
    ///     source: TargetSource::Seed,
    /// };
    /// let (_id, outcome) = store.cache().upsert(&m31, &uuid::Uuid::nil()).await?;
    /// assert_eq!(outcome, UpsertOutcome::Inserted);
    /// # Ok(()) }
    /// ```
    async fn upsert(
        &self,
        identity: &ResolvedIdentity,
        namespace: &Uuid,
    ) -> Result<(Uuid, UpsertOutcome), CacheError>;

    /// Upsert many identities under one `namespace`, in order, with the exact
    /// same per-entry dedup + precedence semantics as calling [`Cache::upsert`]
    /// once per identity (including [`UpsertOutcome::SkippedUserOverride`] and
    /// dedup against earlier entries of the same batch).
    ///
    /// The default implementation simply loops over [`Cache::upsert`]. Warming
    /// a large seed set (thousands of entries) through that default costs one
    /// backend commit per entry; the built-in redb backend
    /// ([`crate::cache::redb::RedbCache`]) overrides this to do the whole batch
    /// in a single write transaction instead.
    ///
    /// ```
    /// use simbad_resolver::{
    ///     AliasKind, Cache, ObjectType, ResolvedAlias, ResolvedIdentity, Store, TargetSource,
    ///     UpsertOutcome,
    /// };
    ///
    /// # async fn run() -> Result<(), simbad_resolver::CacheError> {
    /// let store = Store::in_memory()?;
    /// let m31 = ResolvedIdentity {
    ///     simbad_oid: Some(1_575_544), primary_designation: "M 31".to_owned(),
    ///     common_name: None, object_type: ObjectType::Galaxy, otype_raw: "G".to_owned(),
    ///     ra_deg: 10.684_708, dec_deg: 41.268_75, v_mag: Some(3.44),
    ///     galdim_majaxis_arcmin: None, galdim_minaxis_arcmin: None, galdim_angle_deg: None,
    ///     aliases: vec![ResolvedAlias::new("M 31", AliasKind::Designation)],
    ///     source: TargetSource::Seed,
    /// };
    /// let results = store.cache().upsert_batch(&[m31], &uuid::Uuid::nil()).await?;
    /// assert_eq!(results.len(), 1);
    /// assert_eq!(results[0].1, UpsertOutcome::Inserted);
    /// # Ok(()) }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns the first [`CacheError`] hit; entries already written before
    /// that point in the batch remain committed (the default, per-item
    /// implementation) or are rolled back together (the redb override, which
    /// commits the whole batch atomically).
    async fn upsert_batch(
        &self,
        identities: &[ResolvedIdentity],
        namespace: &Uuid,
    ) -> Result<Vec<(Uuid, UpsertOutcome)>, CacheError> {
        let mut results = Vec::with_capacity(identities.len());
        for identity in identities {
            results.push(self.upsert(identity, namespace).await?);
        }
        Ok(results)
    }

    /// Add a user alias (`kind = 'user'`). Returns `true` if newly inserted,
    /// `false` if it already existed (idempotent).
    ///
    /// ```
    /// use simbad_resolver::{AliasKind, Cache, ObjectType, ResolvedAlias, ResolvedIdentity, Store, TargetSource};
    ///
    /// # async fn run() -> Result<(), simbad_resolver::CacheError> {
    /// let store = Store::in_memory()?;
    /// let m31 = ResolvedIdentity {
    ///     simbad_oid: Some(1_575_544), primary_designation: "M 31".to_owned(),
    ///     common_name: None, object_type: ObjectType::Galaxy, otype_raw: "G".to_owned(),
    ///     ra_deg: 10.684_708, dec_deg: 41.268_75, v_mag: Some(3.44),
    ///     galdim_majaxis_arcmin: None, galdim_minaxis_arcmin: None, galdim_angle_deg: None,
    ///     aliases: vec![ResolvedAlias::new("M 31", AliasKind::Designation)],
    ///     source: TargetSource::Seed,
    /// };
    /// let (id, _) = store.cache().upsert(&m31, &uuid::Uuid::nil()).await?;
    /// assert!(store.cache().add_user_alias(id, "My Andromeda").await?);
    /// assert!(!store.cache().add_user_alias(id, "My Andromeda").await?, "idempotent");
    /// # Ok(()) }
    /// ```
    async fn add_user_alias(&self, target_id: Uuid, alias: &str) -> Result<bool, CacheError>;

    /// Remove a user alias by id, only if its `kind = 'user'`. Returns whether a
    /// row was removed.
    ///
    /// The alias id is opaque and backend-assigned (not exposed by
    /// [`CachedTarget`]/[`ResolvedAlias`] directly), so this trait-level example
    /// only demonstrates the no-op case on an unknown id — see
    /// `crate::cache::redb` tests for a full add/remove round trip.
    ///
    /// ```
    /// use simbad_resolver::{Cache, Store};
    ///
    /// # async fn run() -> Result<(), simbad_resolver::CacheError> {
    /// let store = Store::in_memory()?;
    /// assert!(!store.cache().remove_user_alias("does-not-exist").await?);
    /// # Ok(()) }
    /// ```
    async fn remove_user_alias(&self, alias_id: &str) -> Result<bool, CacheError>;

    /// List all cached targets (ordered by `primary_designation`).
    ///
    /// ```
    /// use simbad_resolver::{AliasKind, Cache, ObjectType, ResolvedAlias, ResolvedIdentity, Store, TargetSource};
    ///
    /// # async fn run() -> Result<(), simbad_resolver::CacheError> {
    /// let store = Store::in_memory()?;
    /// assert!(store.cache().list().await?.is_empty());
    /// let m31 = ResolvedIdentity {
    ///     simbad_oid: Some(1_575_544), primary_designation: "M 31".to_owned(),
    ///     common_name: None, object_type: ObjectType::Galaxy, otype_raw: "G".to_owned(),
    ///     ra_deg: 10.684_708, dec_deg: 41.268_75, v_mag: Some(3.44),
    ///     galdim_majaxis_arcmin: None, galdim_minaxis_arcmin: None, galdim_angle_deg: None,
    ///     aliases: vec![ResolvedAlias::new("M 31", AliasKind::Designation)],
    ///     source: TargetSource::Seed,
    /// };
    /// store.cache().upsert(&m31, &uuid::Uuid::nil()).await?;
    /// assert_eq!(store.cache().list().await?.len(), 1);
    /// # Ok(()) }
    /// ```
    async fn list(&self) -> Result<Vec<CachedTarget>, CacheError>;
}

// ── Queue trait ──────────────────────────────────────────────────────────────

/// The pluggable pending-work store for the async batch resolver.
///
/// Transient failures (`ResolveError::is_transient`) → [`Queue::release`] (stay
/// pending, attempts unchanged). Content misses → [`Queue::mark_unresolved`]
/// (attempts += 1).
#[async_trait::async_trait]
pub trait Queue: Send + Sync {
    /// Enqueue an item (idempotent by `id`); a no-op if `id` already present.
    ///
    /// These trait-method examples all use the built-in [`crate::Store`]'s
    /// in-memory [`Queue`] (`RedbQueue`) — local redb access, no network.
    ///
    /// ```
    /// use simbad_resolver::{Queue, Store};
    ///
    /// # async fn run() -> Result<(), simbad_resolver::Error> {
    /// let store = Store::in_memory()?;
    /// store.queue().enqueue("job-1", "M31").await?;
    /// store.queue().enqueue("job-1", "M31").await?; // idempotent
    /// assert_eq!(store.queue().pending_count().await?, 1);
    /// # Ok(()) }
    /// ```
    async fn enqueue(&self, id: &str, query: &str) -> Result<(), QueueError>;

    /// Claim up to `n` pending items for processing (approximately FIFO).
    ///
    /// ```
    /// use simbad_resolver::{PendingState, Queue, Store};
    ///
    /// # async fn run() -> Result<(), simbad_resolver::Error> {
    /// let store = Store::in_memory()?;
    /// store.queue().enqueue("job-1", "M31").await?;
    /// let claimed = store.queue().claim_pending(8).await?;
    /// assert_eq!(claimed.len(), 1);
    /// assert_eq!(claimed[0].state, PendingState::Pending);
    /// # Ok(()) }
    /// ```
    async fn claim_pending(&self, n: usize) -> Result<Vec<PendingItem>, QueueError>;

    /// Mark an item resolved and bind its target (attempts unchanged).
    ///
    /// ```
    /// use simbad_resolver::{PendingState, Queue, Store};
    /// use uuid::Uuid;
    ///
    /// # async fn run() -> Result<(), simbad_resolver::Error> {
    /// let store = Store::in_memory()?;
    /// store.queue().enqueue("job-1", "M31").await?;
    /// store.queue().mark_resolved("job-1", Uuid::nil()).await?;
    /// let item = store.queue().get("job-1").await?.expect("still present");
    /// assert_eq!(item.state, PendingState::Resolved);
    /// assert_eq!(item.target_id, Some(Uuid::nil()));
    /// # Ok(()) }
    /// ```
    async fn mark_resolved(&self, id: &str, target_id: Uuid) -> Result<(), QueueError>;

    /// Mark an item unresolved (content miss); attempts += 1.
    ///
    /// ```
    /// use simbad_resolver::{PendingState, Queue, Store};
    ///
    /// # async fn run() -> Result<(), simbad_resolver::Error> {
    /// let store = Store::in_memory()?;
    /// store.queue().enqueue("job-1", "does-not-exist").await?;
    /// store.queue().mark_unresolved("job-1").await?;
    /// let item = store.queue().get("job-1").await?.expect("still present");
    /// assert_eq!(item.state, PendingState::Unresolved);
    /// assert_eq!(item.attempts, 1);
    /// # Ok(()) }
    /// ```
    async fn mark_unresolved(&self, id: &str) -> Result<(), QueueError>;

    /// Release a claimed item back to pending after a transient failure
    /// (attempts unchanged).
    ///
    /// ```
    /// use simbad_resolver::{PendingState, Queue, Store};
    ///
    /// # async fn run() -> Result<(), simbad_resolver::Error> {
    /// let store = Store::in_memory()?;
    /// store.queue().enqueue("job-1", "M31").await?;
    /// store.queue().claim_pending(8).await?; // simulate a claim
    /// store.queue().release("job-1").await?; // ... then a transient failure
    /// let item = store.queue().get("job-1").await?.expect("still present");
    /// assert_eq!(item.state, PendingState::Pending);
    /// assert_eq!(item.attempts, 0, "transient failures don't spend the attempt budget");
    /// # Ok(()) }
    /// ```
    async fn release(&self, id: &str) -> Result<(), QueueError>;

    /// Read a single item by id.
    ///
    /// ```
    /// use simbad_resolver::{Queue, Store};
    ///
    /// # async fn run() -> Result<(), simbad_resolver::Error> {
    /// let store = Store::in_memory()?;
    /// assert!(store.queue().get("job-1").await?.is_none());
    /// store.queue().enqueue("job-1", "M31").await?;
    /// assert_eq!(store.queue().get("job-1").await?.unwrap().query, "M31");
    /// # Ok(()) }
    /// ```
    async fn get(&self, id: &str) -> Result<Option<PendingItem>, QueueError>;

    /// Count items still `pending`.
    ///
    /// ```
    /// use simbad_resolver::{Queue, Store};
    /// use uuid::Uuid;
    ///
    /// # async fn run() -> Result<(), simbad_resolver::Error> {
    /// let store = Store::in_memory()?;
    /// store.queue().enqueue("job-1", "M31").await?;
    /// store.queue().enqueue("job-2", "M101").await?;
    /// store.queue().mark_resolved("job-1", Uuid::nil()).await?;
    /// assert_eq!(store.queue().pending_count().await?, 1);
    /// # Ok(()) }
    /// ```
    async fn pending_count(&self) -> Result<usize, QueueError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AliasKind;

    #[test]
    fn pending_state_wire_round_trips() {
        for st in [PendingState::Pending, PendingState::Resolved, PendingState::Unresolved] {
            assert_eq!(PendingState::from_wire(st.as_wire()), Some(st));
        }
        assert_eq!(PendingState::Pending.as_wire(), "pending");
        assert_eq!(PendingState::Resolved.as_wire(), "resolved");
        assert_eq!(PendingState::Unresolved.as_wire(), "unresolved");
        assert_eq!(PendingState::from_wire("bogus"), None);
        assert_eq!(PendingState::from_wire(""), None);
    }

    #[test]
    fn cached_target_to_identity_preserves_the_identity_fields() {
        let target = CachedTarget {
            id: Uuid::nil(),
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
            source: TargetSource::Resolved,
            resolved_at: "2026-07-11T00:00:00Z".to_owned(),
            aliases: vec![ResolvedAlias::new("NGC 224", AliasKind::Designation)],
        };

        let identity = target.to_identity();
        assert_eq!(identity.simbad_oid, Some(1_575_544));
        assert_eq!(identity.primary_designation, "M 31");
        assert_eq!(identity.common_name.as_deref(), Some("Andromeda Galaxy"));
        assert_eq!(identity.object_type, ObjectType::Galaxy);
        assert_eq!(identity.otype_raw, "G");
        assert_eq!(identity.v_mag, Some(3.44));
        assert_eq!(identity.galdim_majaxis_arcmin, Some(199.53));
        assert_eq!(identity.galdim_minaxis_arcmin, Some(70.79));
        assert_eq!(identity.galdim_angle_deg, Some(35));
        assert!((identity.ra_deg - 10.684_708).abs() < f64::EPSILON);
        assert_eq!(identity.source, TargetSource::Resolved);
        assert_eq!(identity.aliases, target.aliases);
    }
}
