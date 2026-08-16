// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Single redb-backed [`Cache`] + [`Queue`], durable (file) or ephemeral
//! (in-memory).
//!
//! # Table design
//!
//! redb keeps keys sorted; every table is keyed by a `&str` or `&str`-encoded
//! id and stores a `serde_json` blob, mirroring the in-memory backend's
//! indexes:
//!
//! - `targets` — `id (uuid) → StoredTarget` (the canonical row, aliases
//!   excluded). Dedup by `simbad_oid` and lookup-by-oid scan this table (the
//!   oid lives inside the row, so no separate oid index can go stale).
//! - `aliases` — `alias_id (uuid) → StoredAlias { target_id, alias }`, the
//!   single source of truth for every alias. `get_by_normalized`, `search`, and
//!   per-target assembly scan this table (a local typeahead cache is small, and
//!   a full scan matches the reference backend's behaviour exactly).
//! - `pending` — `id → StoredPending`, the batch queue; a monotonic `seq`
//!   (allocated from `meta`) gives `claim_pending` its approximate-FIFO order.
//! - `meta` — `&str → u64`; holds the next pending sequence number.
//!
//! redb is synchronous; the async trait methods run each unit of work inside
//! [`tokio::task::spawn_blocking`], moving a cloned `Arc<Database>` in.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    Cache, CacheError, CachedTarget, PendingItem, PendingState, Queue, QueueError, SearchHit,
    UpsertOutcome, RANK_EXACT, RANK_PREFIX, RANK_SUBSTRING,
};
use crate::identity::target_id_from_designation;
use crate::normalize::normalize;
use crate::types::{AliasKind, ObjectType, ResolvedAlias, ResolvedIdentity, TargetSource};

const TARGETS: TableDefinition<&str, &[u8]> = TableDefinition::new("targets");
const ALIASES: TableDefinition<&str, &[u8]> = TableDefinition::new("aliases");
const PENDING: TableDefinition<&str, &[u8]> = TableDefinition::new("pending");
const META: TableDefinition<&str, u64> = TableDefinition::new("meta");
const PENDING_SEQ_KEY: &str = "pending_seq";

/// Map any `Display` error (redb, serde, join) to a [`CacheError::Backend`].
fn cache_err<E: std::fmt::Display>(e: E) -> CacheError {
    CacheError::Backend(e.to_string())
}

/// Map any `Display` error (redb, serde, join) to a [`QueueError::Backend`].
fn queue_err<E: std::fmt::Display>(e: E) -> QueueError {
    QueueError::Backend(e.to_string())
}

/// The current time as an RFC 3339 timestamp (UTC).
fn now_iso() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .expect("Rfc3339 format is always valid for OffsetDateTime")
}

// ── Persisted row structs ─────────────────────────────────────────────────────

/// A canonical target row (aliases excluded — those live in the `aliases`
/// table, keyed independently so `remove_user_alias` can address one).
#[derive(Serialize, Deserialize)]
struct StoredTarget {
    simbad_oid: Option<i64>,
    primary_designation: String,
    common_name: Option<String>,
    object_type: ObjectType,
    otype_raw: String,
    ra_deg: f64,
    dec_deg: f64,
    /// Johnson V magnitude when known. `#[serde(default)]` so target rows
    /// written before this field existed still deserialize (as `None`).
    #[serde(default)]
    v_mag: Option<f64>,
    /// Angular major-axis diameter, arcminutes, unconverted. `#[serde(default)]`
    /// for the same forward-compat reason as `v_mag`: a row written before this
    /// field existed still deserializes, reading back as `None` (not `0.0`) so
    /// "no measurement" is never conflated with "measured as zero". See
    /// `galdim_pre_upgrade_row_deserializes_with_none` for the read-old-write-new
    /// proof.
    #[serde(default)]
    galdim_majaxis_arcmin: Option<f64>,
    /// Angular minor-axis diameter, arcminutes, unconverted. Same
    /// `#[serde(default)]` compat rule as [`Self::galdim_majaxis_arcmin`].
    #[serde(default)]
    galdim_minaxis_arcmin: Option<f64>,
    /// Position angle of the major axis, degrees, unconverted. Same
    /// `#[serde(default)]` compat rule as [`Self::galdim_majaxis_arcmin`].
    #[serde(default)]
    galdim_angle_deg: Option<i16>,
    source: TargetSource,
    resolved_at: String,
}

impl StoredTarget {
    fn from_identity(identity: &ResolvedIdentity) -> Self {
        Self {
            simbad_oid: identity.simbad_oid,
            primary_designation: identity.primary_designation.clone(),
            common_name: identity.common_name.clone(),
            object_type: identity.object_type,
            otype_raw: identity.otype_raw.clone(),
            ra_deg: identity.ra_deg,
            dec_deg: identity.dec_deg,
            v_mag: identity.v_mag,
            galdim_majaxis_arcmin: identity.galdim_majaxis_arcmin,
            galdim_minaxis_arcmin: identity.galdim_minaxis_arcmin,
            galdim_angle_deg: identity.galdim_angle_deg,
            source: identity.source,
            resolved_at: now_iso(),
        }
    }

    fn into_target(self, id: Uuid, aliases: Vec<ResolvedAlias>) -> CachedTarget {
        CachedTarget {
            id,
            simbad_oid: self.simbad_oid,
            primary_designation: self.primary_designation,
            common_name: self.common_name,
            object_type: self.object_type,
            otype_raw: self.otype_raw,
            ra_deg: self.ra_deg,
            dec_deg: self.dec_deg,
            v_mag: self.v_mag,
            galdim_majaxis_arcmin: self.galdim_majaxis_arcmin,
            galdim_minaxis_arcmin: self.galdim_minaxis_arcmin,
            galdim_angle_deg: self.galdim_angle_deg,
            source: self.source,
            resolved_at: self.resolved_at,
            aliases,
        }
    }
}

/// One alias row: a [`ResolvedAlias`] owned by `target_id`.
#[derive(Serialize, Deserialize)]
struct StoredAlias {
    target_id: String,
    alias: ResolvedAlias,
}

/// One queued item. `state` is the [`PendingState`] wire string; `seq` is the
/// monotonic insertion order used to approximate FIFO in `claim_pending`.
#[derive(Serialize, Deserialize)]
struct StoredPending {
    query: String,
    state: String,
    attempts: i64,
    target_id: Option<String>,
    seq: u64,
}

impl StoredPending {
    fn into_item(self, id: String) -> Result<PendingItem, QueueError> {
        let state = PendingState::from_wire(&self.state)
            .ok_or_else(|| QueueError::InvalidState(self.state.clone()))?;
        let target_id = self
            .target_id
            .map(|t| Uuid::parse_str(&t).map_err(|e| QueueError::InvalidUuid(t.clone(), e)))
            .transpose()?;
        Ok(PendingItem { id, query: self.query, state, attempts: self.attempts, target_id })
    }
}

// ── Store ─────────────────────────────────────────────────────────────────────

/// Write-transaction durability [`Cache::upsert_batch`] uses, configured once
/// when a [`Store`] is opened (see [`Store::open_with`]) — not per call.
/// Maps to `redb::Durability`: `Durable` -> `Immediate`, `Eventual` -> `None`.
/// Single-item [`Cache::upsert`] always stays `Durable` regardless of this
/// setting — point writes should stay durable; only a bulk batch load is
/// worth trading that away.
///
/// Per redb's docs, a `None`-durability commit is visible to later
/// reads/writes against the same open [`Database`] immediately, but "will not
/// be persisted to disk unless followed by a commit with
/// [`redb::Durability::Immediate`]" — so **only an unclean process
/// termination** (a real crash/power-loss) *before* that later durable commit
/// can roll it back. A clean shutdown is not at risk: redb's [`Database`]
/// flushes and finalizes its on-disk header on `Drop` regardless of the last
/// commit's durability, so an `Eventual` write left uncommitted-durably at
/// process exit is still persisted once the process exits normally — the gap
/// [`RedbCache::flush`] closes is the crash window *while the `Database`
/// stays open*, not "forgetting to flush before exit".
///
/// Intended usage for a chunked bulk load (nightwatch-astro/alm#695: a
/// 13k-entry seed warm split into e.g. 1000-entry chunks): open the store
/// with `Eventual`, call [`Cache::upsert_batch`] once per chunk (none of them
/// fsync), then call [`RedbCache::flush`] once right after the last chunk —
/// don't wait for the store to close naturally, since it may stay open for
/// the rest of the process's lifetime. redb commits are cumulative — each
/// transaction's root builds on the last — so that one fsync persists every
/// chunk, not just the last one.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BatchDurability {
    /// Every `upsert_batch` commit is fsync'd — today's only behavior.
    #[default]
    Durable,
    /// `upsert_batch` commits skip the fsync; call [`RedbCache::flush`] once
    /// after the last chunk to persist all of them in a single fsync.
    Eventual,
}

/// An open redb database shared by a [`RedbCache`] and a [`RedbQueue`].
///
/// Cloning is cheap (an `Arc` handle over one `redb::Database`), so
/// [`Self::cache`] / [`Self::queue`] hand out independent wrappers sharing the
/// same underlying database.
///
/// [`Store::in_memory`] opens an ephemeral database, so this example is fully
/// runnable with no filesystem access. Share one [`Store`] between a
/// [`crate::SimbadResolver`] (via [`crate::CacheBackend::custom`]) and a
/// [`crate::BatchResolver`] to have both operate over the same rows.
///
/// ```
/// use simbad_resolver::Store;
///
/// # fn demo() -> Result<(), simbad_resolver::CacheError> {
/// let store = Store::in_memory()?;
/// let cache = store.cache(); // implements `Cache`
/// let queue = store.queue(); // implements `Queue`
/// # let _ = (cache, queue);
/// # Ok(()) }
/// ```
#[derive(Clone)]
pub struct Store {
    db: Arc<Database>,
    bulk_durability: BatchDurability,
}

impl Store {
    /// Open (creating if missing) a durable, file-backed store at `path`,
    /// with the default [`BatchDurability::Durable`] bulk-load policy —
    /// exactly today's behavior. See [`Store::open_with`] to opt into
    /// [`BatchDurability::Eventual`] batch loads.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::Backend`] if the database cannot be opened or its
    /// tables cannot be initialised.
    ///
    /// ```
    /// use simbad_resolver::Store;
    ///
    /// # fn run() -> Result<(), simbad_resolver::CacheError> {
    /// let path = std::env::temp_dir().join("simbad-resolver-doctest-store-open.redb");
    /// let store = Store::open(&path)?; // creates the file if missing
    /// assert!(path.exists());
    /// # let _ = store;
    /// # let _ = std::fs::remove_file(&path);
    /// # Ok(()) }
    /// ```
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CacheError> {
        Self::open_with(path, BatchDurability::default())
    }

    /// Like [`Store::open`], but configuring the [`BatchDurability`] every
    /// [`Cache::upsert_batch`] call against this store uses.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::Backend`] if the database cannot be opened or its
    /// tables cannot be initialised.
    pub fn open_with(
        path: impl AsRef<Path>,
        bulk_durability: BatchDurability,
    ) -> Result<Self, CacheError> {
        let db = Database::create(path).map_err(cache_err)?;
        init_tables(&db)?;
        Ok(Self { db: Arc::new(db), bulk_durability })
    }

    /// Open a fresh, ephemeral in-memory store (nothing persisted to disk),
    /// with the default [`BatchDurability::Durable`] bulk-load policy.
    ///
    /// [`BatchDurability::Eventual`] only matters for a file-backed store
    /// (there's no "reopen after a crash" scenario for an in-memory one), so
    /// unlike [`Store::open`] this has no `_with` variant; add one if that
    /// changes.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::Backend`] if the in-memory database cannot be
    /// created or its tables cannot be initialised.
    pub fn in_memory() -> Result<Self, CacheError> {
        let db = Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .map_err(cache_err)?;
        init_tables(&db)?;
        Ok(Self { db: Arc::new(db), bulk_durability: BatchDurability::default() })
    }

    /// A [`RedbCache`] over this store's database, inheriting its configured
    /// [`BatchDurability`].
    #[must_use]
    pub fn cache(&self) -> RedbCache {
        RedbCache { db: self.db.clone(), bulk_durability: self.bulk_durability }
    }

    /// A [`RedbQueue`] over this store's database.
    #[must_use]
    pub fn queue(&self) -> RedbQueue {
        RedbQueue { db: self.db.clone() }
    }
}

/// Create every table up front so later read transactions always find them
/// (redb errors on `open_table` for a table that was never written).
fn init_tables(db: &Database) -> Result<(), CacheError> {
    let w = db.begin_write().map_err(cache_err)?;
    {
        w.open_table(TARGETS).map_err(cache_err)?;
        w.open_table(ALIASES).map_err(cache_err)?;
        w.open_table(PENDING).map_err(cache_err)?;
        w.open_table(META).map_err(cache_err)?;
    }
    w.commit().map_err(cache_err)?;
    Ok(())
}

/// The durable/ephemeral redb-backed [`Cache`].
#[derive(Clone)]
pub struct RedbCache {
    db: Arc<Database>,
    bulk_durability: BatchDurability,
}

/// The durable/ephemeral redb-backed [`Queue`].
#[derive(Clone)]
pub struct RedbQueue {
    db: Arc<Database>,
}

// ── Cache sync operations ─────────────────────────────────────────────────────

/// Load every alias owned by `target_id`, sorted by display form.
fn aliases_for(db: &Database, target_id: &str) -> Result<Vec<ResolvedAlias>, CacheError> {
    let txn = db.begin_read().map_err(cache_err)?;
    let table = txn.open_table(ALIASES).map_err(cache_err)?;
    let mut aliases = Vec::new();
    for entry in table.iter().map_err(cache_err)? {
        let (_, v) = entry.map_err(cache_err)?;
        let sa: StoredAlias = serde_json::from_slice(v.value()).map_err(cache_err)?;
        if sa.target_id == target_id {
            aliases.push(sa.alias);
        }
    }
    aliases.sort_by(|a, b| a.alias.cmp(&b.alias));
    Ok(aliases)
}

fn get_by_id(db: &Database, id: Uuid) -> Result<Option<CachedTarget>, CacheError> {
    let id_str = id.to_string();
    let txn = db.begin_read().map_err(cache_err)?;
    let targets = txn.open_table(TARGETS).map_err(cache_err)?;
    let Some(bytes) = targets.get(id_str.as_str()).map_err(cache_err)? else {
        return Ok(None);
    };
    let st: StoredTarget = serde_json::from_slice(bytes.value()).map_err(cache_err)?;
    drop(bytes);
    let aliases = aliases_for(db, &id_str)?;
    Ok(Some(st.into_target(id, aliases)))
}

fn get_by_simbad_oid(db: &Database, oid: i64) -> Result<Option<CachedTarget>, CacheError> {
    let found = {
        let txn = db.begin_read().map_err(cache_err)?;
        let targets = txn.open_table(TARGETS).map_err(cache_err)?;
        let mut hit: Option<String> = None;
        for entry in targets.iter().map_err(cache_err)? {
            let (k, v) = entry.map_err(cache_err)?;
            let st: StoredTarget = serde_json::from_slice(v.value()).map_err(cache_err)?;
            if st.simbad_oid == Some(oid) {
                hit = Some(k.value().to_string());
                break;
            }
        }
        hit
    };
    match found {
        None => Ok(None),
        Some(id_str) => {
            let id = Uuid::parse_str(&id_str).map_err(|e| CacheError::InvalidUuid(id_str, e))?;
            get_by_id(db, id)
        }
    }
}

fn get_by_normalized(db: &Database, normalized: &str) -> Result<Option<CachedTarget>, CacheError> {
    let found = {
        let txn = db.begin_read().map_err(cache_err)?;
        let table = txn.open_table(ALIASES).map_err(cache_err)?;
        let mut hit: Option<String> = None;
        for entry in table.iter().map_err(cache_err)? {
            let (_, v) = entry.map_err(cache_err)?;
            let sa: StoredAlias = serde_json::from_slice(v.value()).map_err(cache_err)?;
            if sa.alias.normalized == normalized {
                hit = Some(sa.target_id);
                break;
            }
        }
        hit
    };
    match found {
        None => Ok(None),
        Some(id_str) => {
            let id = Uuid::parse_str(&id_str).map_err(|e| CacheError::InvalidUuid(id_str, e))?;
            get_by_id(db, id)
        }
    }
}

/// The best-ranked alias hit seen so far for one target during search dedup.
struct Best {
    alias: String,
    normalized_len: usize,
    rank: u8,
}

impl Best {
    /// A lower rank wins; ties break on the shorter matched alias.
    fn is_better_than(&self, other: &Self) -> bool {
        (self.rank, self.normalized_len) < (other.rank, other.normalized_len)
    }
}

fn search(db: &Database, query: &str, limit: usize) -> Result<Vec<SearchHit>, CacheError> {
    let q = normalize(query);
    if q.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let mut best_by_target: HashMap<String, Best> = HashMap::new();
    {
        let txn = db.begin_read().map_err(cache_err)?;
        let table = txn.open_table(ALIASES).map_err(cache_err)?;
        for entry in table.iter().map_err(cache_err)? {
            let (_, v) = entry.map_err(cache_err)?;
            let sa: StoredAlias = serde_json::from_slice(v.value()).map_err(cache_err)?;
            let normalized = &sa.alias.normalized;
            let rank = if *normalized == q {
                RANK_EXACT
            } else if normalized.starts_with(&q) {
                RANK_PREFIX
            } else if normalized.contains(&q) {
                RANK_SUBSTRING
            } else {
                continue;
            };
            let candidate =
                Best { alias: sa.alias.alias.clone(), normalized_len: normalized.len(), rank };
            match best_by_target.entry(sa.target_id) {
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    if candidate.is_better_than(e.get()) {
                        e.insert(candidate);
                    }
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(candidate);
                }
            }
        }
    }

    let mut ranked: Vec<(String, Best)> = best_by_target.into_iter().collect();
    ranked.sort_by(|(_, a), (_, b)| {
        (a.rank, a.normalized_len, a.alias.as_str()).cmp(&(
            b.rank,
            b.normalized_len,
            b.alias.as_str(),
        ))
    });
    ranked.truncate(limit);

    let mut hits = Vec::with_capacity(ranked.len());
    for (target_id, best) in ranked {
        let id = Uuid::parse_str(&target_id).map_err(|e| CacheError::InvalidUuid(target_id, e))?;
        if let Some(target) = get_by_id(db, id)? {
            hits.push(SearchHit { target, matched_alias: best.alias, rank: best.rank });
        }
    }
    Ok(hits)
}

/// How [`rewrite_aliases`] finds a target's current alias row ids to retire.
///
/// Mirrors [`OidLookup`]: `Scan` (single-item `upsert`, one call so an O(n)
/// walk of `aliases` is fine) vs. `Index` (`upsert_batch`, an O(1) lookup
/// against a batch-local `target_id -> alias ids` index built once up front
/// by [`build_alias_index`] and kept in sync). Without this, `upsert_batch`'s
/// oid-index fix alone still left this scan re-walking the (growing)
/// `aliases` table once per entry — the other half of alm#695's O(n²).
enum AliasLookup<'a> {
    Scan,
    Index(&'a mut HashMap<String, Vec<String>>),
}

/// Replace all alias rows for `target_id` wholesale, ensuring the primary
/// designation is present as a `designation` alias and deduping by normalized
/// form (mirrors the in-memory backend's `rewrite_aliases`).
fn rewrite_aliases(
    w: &redb::WriteTransaction,
    target_id: &str,
    identity: &ResolvedIdentity,
    alias_lookup: &mut AliasLookup,
) -> Result<(), CacheError> {
    let mut table = w.open_table(ALIASES).map_err(cache_err)?;

    let stale: Vec<String> = match alias_lookup {
        AliasLookup::Scan => {
            #[cfg(test)]
            FULL_TABLE_SCAN_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut keys = Vec::new();
            for entry in table.iter().map_err(cache_err)? {
                let (k, v) = entry.map_err(cache_err)?;
                let sa: StoredAlias = serde_json::from_slice(v.value()).map_err(cache_err)?;
                if sa.target_id == target_id {
                    keys.push(k.value().to_string());
                }
            }
            keys
        }
        AliasLookup::Index(index) => index.remove(target_id).unwrap_or_default(),
    };
    for k in &stale {
        table.remove(k.as_str()).map_err(cache_err)?;
    }

    let mut aliases = identity.aliases.clone();
    let primary_norm = normalize(&identity.primary_designation);
    if !aliases.iter().any(|a| a.normalized == primary_norm) {
        aliases
            .push(ResolvedAlias::new(identity.primary_designation.clone(), AliasKind::Designation));
    }

    let mut seen = HashSet::with_capacity(aliases.len());
    let mut written = Vec::with_capacity(aliases.len());
    for alias in aliases {
        if !seen.insert(alias.normalized.clone()) {
            continue; // tolerate duplicate normalized forms within one identity
        }
        let alias_id = Uuid::new_v4().to_string();
        let sa = StoredAlias { target_id: target_id.to_owned(), alias };
        let bytes = serde_json::to_vec(&sa).map_err(cache_err)?;
        table.insert(alias_id.as_str(), bytes.as_slice()).map_err(cache_err)?;
        written.push(alias_id);
    }

    if let AliasLookup::Index(index) = alias_lookup {
        index.insert(target_id.to_owned(), written);
    }
    Ok(())
}

/// One full scan of `aliases`, indexing every row's owning `target_id`.
/// `upsert_batch` runs this exactly once per batch — the alias-side twin of
/// [`build_oid_index`], fixing the other half of the O(n²) blowup.
fn build_alias_index(
    w: &redb::WriteTransaction,
) -> Result<HashMap<String, Vec<String>>, CacheError> {
    #[cfg(test)]
    FULL_TABLE_SCAN_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let table = w.open_table(ALIASES).map_err(cache_err)?;
    let mut index: HashMap<String, Vec<String>> = HashMap::new();
    for entry in table.iter().map_err(cache_err)? {
        let (k, v) = entry.map_err(cache_err)?;
        let sa: StoredAlias = serde_json::from_slice(v.value()).map_err(cache_err)?;
        index.entry(sa.target_id).or_default().push(k.value().to_string());
    }
    Ok(index)
}

/// Test-only: counts every full-table scan done to find a target by oid
/// (`OidLookup::Scan` / [`build_oid_index`]) or a target's aliases
/// (`AliasLookup::Scan` / [`build_alias_index`]), so a unit test can assert
/// `upsert_batch` collapses those scans to one each regardless of batch size
/// (see the `upsert_batch_*` tests below). Zero cost outside test builds.
#[cfg(test)]
static FULL_TABLE_SCAN_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How [`upsert_within`] finds a row that already carries a given SIMBAD oid.
///
/// A single [`upsert`] call scans (`Scan`): one call, so an O(n) walk of
/// `targets` is fine and this keeps that path byte-for-byte what it always
/// was. [`upsert_batch`] instead builds one O(1) `oid -> (id, source)` index
/// up front (`Index`, via [`build_oid_index`]) and keeps it in sync as the
/// batch writes — the fix for nightwatch-astro/alm#695's O(n²) blowup: an
/// n-entry batch used to re-scan the (growing) table once per entry.
enum OidLookup<'a> {
    /// Full-table scan, once per call.
    Scan,
    /// O(1) lookup against a batch-local index, updated after every write so
    /// later entries in the same batch see accurate dedup state.
    Index(&'a mut HashMap<i64, (String, TargetSource)>),
}

/// Upsert one identity against tables already open in `w`, without
/// committing. Shared by [`upsert`] (one identity, one transaction) and
/// [`upsert_batch`] (many identities, one transaction) so a batch gets the
/// exact same per-entry dedup + precedence decision as a sequential loop of
/// single upserts — including deduping against an earlier identity of the
/// same batch, since reads here see this transaction's own uncommitted
/// writes.
fn upsert_within(
    w: &redb::WriteTransaction,
    identity: &ResolvedIdentity,
    namespace: &Uuid,
    oid_lookup: &mut OidLookup,
    alias_lookup: &mut AliasLookup,
) -> Result<(Uuid, UpsertOutcome), CacheError> {
    let derived_id =
        target_id_from_designation(namespace, &identity.primary_designation).to_string();

    // `existing`'s third field is the row's oid *before* this write (`None` if
    // it never had one), so the tail below can retire a stale index entry if
    // this write changes or clears the row's oid.
    let (id_str, outcome, wrote_row, prior_oid) = {
        let mut targets = w.open_table(TARGETS).map_err(cache_err)?;

        // Find the row to upsert into: by oid when Some, else by derived id.
        let mut existing: Option<(String, TargetSource, Option<i64>)> = None;
        if let Some(oid) = identity.simbad_oid {
            existing = match oid_lookup {
                OidLookup::Scan => {
                    #[cfg(test)]
                    FULL_TABLE_SCAN_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let mut found = None;
                    for entry in targets.iter().map_err(cache_err)? {
                        let (k, v) = entry.map_err(cache_err)?;
                        let st: StoredTarget =
                            serde_json::from_slice(v.value()).map_err(cache_err)?;
                        if st.simbad_oid == Some(oid) {
                            found = Some((k.value().to_string(), st.source, Some(oid)));
                            break;
                        }
                    }
                    found
                }
                OidLookup::Index(index) => {
                    index.get(&oid).map(|(id, source)| (id.clone(), *source, Some(oid)))
                }
            };
        }
        if existing.is_none() {
            if let Some(v) = targets.get(derived_id.as_str()).map_err(cache_err)? {
                let st: StoredTarget = serde_json::from_slice(v.value()).map_err(cache_err)?;
                existing = Some((derived_id.clone(), st.source, st.simbad_oid));
            }
        }

        match existing {
            Some((id, source, _)) if !identity.source.may_overwrite(source) => {
                (id, UpsertOutcome::SkippedUserOverride, false, None)
            }
            Some((id, _, prior_oid)) => {
                let bytes = serde_json::to_vec(&StoredTarget::from_identity(identity))
                    .map_err(cache_err)?;
                targets.insert(id.as_str(), bytes.as_slice()).map_err(cache_err)?;
                (id, UpsertOutcome::Updated, true, prior_oid)
            }
            None => {
                let bytes = serde_json::to_vec(&StoredTarget::from_identity(identity))
                    .map_err(cache_err)?;
                targets.insert(derived_id.as_str(), bytes.as_slice()).map_err(cache_err)?;
                (derived_id.clone(), UpsertOutcome::Inserted, true, None)
            }
        }
    };

    if wrote_row {
        rewrite_aliases(w, &id_str, identity, alias_lookup)?;
        if let OidLookup::Index(index) = oid_lookup {
            if prior_oid != identity.simbad_oid {
                if let Some(stale) = prior_oid {
                    index.remove(&stale);
                }
            }
            if let Some(oid) = identity.simbad_oid {
                index.insert(oid, (id_str.clone(), identity.source));
            }
        }
    }

    let id = Uuid::parse_str(&id_str).map_err(|e| CacheError::InvalidUuid(id_str, e))?;
    Ok((id, outcome))
}

/// One full scan of `targets`, indexing every row that currently has an oid.
/// `upsert_batch` runs this exactly once per batch (not once per entry) —
/// that single scan is what turns an n-entry batch's total table-scan work
/// from O(n²) back to O(n).
fn build_oid_index(
    w: &redb::WriteTransaction,
) -> Result<HashMap<i64, (String, TargetSource)>, CacheError> {
    #[cfg(test)]
    FULL_TABLE_SCAN_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let targets = w.open_table(TARGETS).map_err(cache_err)?;
    let mut index = HashMap::new();
    for entry in targets.iter().map_err(cache_err)? {
        let (k, v) = entry.map_err(cache_err)?;
        let st: StoredTarget = serde_json::from_slice(v.value()).map_err(cache_err)?;
        if let Some(oid) = st.simbad_oid {
            index.insert(oid, (k.value().to_string(), st.source));
        }
    }
    Ok(index)
}

/// Test-only: counts write transactions opened for an upsert (single or
/// batch), so a unit test can assert `upsert_batch` collapses N transactions
/// into 1 (see the `upsert_batch_*` tests below). Zero cost outside test
/// builds.
#[cfg(test)]
static UPSERT_WRITE_TXN_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Begin the one write transaction an upsert (single or batch) runs in,
/// counting it in test builds via [`UPSERT_WRITE_TXN_COUNT`].
fn begin_upsert_txn(db: &Database) -> Result<redb::WriteTransaction, CacheError> {
    #[cfg(test)]
    UPSERT_WRITE_TXN_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    db.begin_write().map_err(cache_err)
}

fn upsert(
    db: &Database,
    identity: &ResolvedIdentity,
    namespace: &Uuid,
) -> Result<(Uuid, UpsertOutcome), CacheError> {
    let w = begin_upsert_txn(db)?;
    let result =
        upsert_within(&w, identity, namespace, &mut OidLookup::Scan, &mut AliasLookup::Scan)?;
    w.commit().map_err(cache_err)?;
    Ok(result)
}

/// Upsert every identity in `identities` in one write transaction (one
/// backend commit total instead of one per entry — see `docs/adr/` and issue
/// nightwatch-astro/alm#695 for why this matters for large seed warms), at
/// the store's configured [`BatchDurability`] (see [`Store::open_with`]).
///
/// Per-entry semantics are identical to calling [`upsert`] in a loop: each
/// entry is deduped/precedence-checked against both already-committed rows and
/// rows written earlier in this same batch. If any entry errors, the whole
/// transaction is dropped unread (redb aborts an uncommitted write
/// transaction), so a batch either fully applies or leaves the store
/// untouched.
fn upsert_batch(
    db: &Database,
    identities: &[ResolvedIdentity],
    namespace: &Uuid,
    durability: BatchDurability,
) -> Result<Vec<(Uuid, UpsertOutcome)>, CacheError> {
    let mut w = begin_upsert_txn(db)?;
    // Only touch durability for the non-default case, so the `Durable` path
    // is byte-for-byte what it always was (no new fallible call on it).
    if durability == BatchDurability::Eventual {
        w.set_durability(redb::Durability::None).map_err(cache_err)?;
    }
    let mut oid_index = build_oid_index(&w)?;
    let mut alias_index = build_alias_index(&w)?;
    let mut results = Vec::with_capacity(identities.len());
    for identity in identities {
        results.push(upsert_within(
            &w,
            identity,
            namespace,
            &mut OidLookup::Index(&mut oid_index),
            &mut AliasLookup::Index(&mut alias_index),
        )?);
    }
    w.commit().map_err(cache_err)?;
    Ok(results)
}

/// Force one fully durable (fsync'd) write commit, touching no data. redb
/// commits are cumulative — each transaction's root builds on the last — so
/// this persists every prior [`BatchDurability::Eventual`] `upsert_batch`
/// commit too, not just anything new. See [`RedbCache::flush`].
fn flush_durable(db: &Database) -> Result<(), CacheError> {
    let w = db.begin_write().map_err(cache_err)?;
    w.commit().map_err(cache_err)?;
    Ok(())
}

fn add_user_alias(db: &Database, target_id: Uuid, alias: &str) -> Result<bool, CacheError> {
    let normalized = normalize(alias);
    if normalized.is_empty() {
        return Ok(false);
    }
    let tid = target_id.to_string();
    let w = db.begin_write().map_err(cache_err)?;
    let inserted = {
        let mut table = w.open_table(ALIASES).map_err(cache_err)?;
        let exists = {
            let mut found = false;
            for entry in table.iter().map_err(cache_err)? {
                let (_, v) = entry.map_err(cache_err)?;
                let sa: StoredAlias = serde_json::from_slice(v.value()).map_err(cache_err)?;
                if sa.target_id == tid && sa.alias.normalized == normalized {
                    found = true;
                    break;
                }
            }
            found
        };
        if exists {
            false
        } else {
            let alias_id = Uuid::new_v4().to_string();
            let sa = StoredAlias {
                target_id: tid.clone(),
                alias: ResolvedAlias::new(alias, AliasKind::User),
            };
            let bytes = serde_json::to_vec(&sa).map_err(cache_err)?;
            table.insert(alias_id.as_str(), bytes.as_slice()).map_err(cache_err)?;
            true
        }
    };
    w.commit().map_err(cache_err)?;
    Ok(inserted)
}

fn remove_user_alias(db: &Database, alias_id: &str) -> Result<bool, CacheError> {
    let w = db.begin_write().map_err(cache_err)?;
    let removed = {
        let mut table = w.open_table(ALIASES).map_err(cache_err)?;
        // Copy the row out so the read guard is dropped before the write borrow.
        let existing = table.get(alias_id).map_err(cache_err)?.map(|v| v.value().to_vec());
        match existing {
            None => false,
            Some(raw) => {
                let sa: StoredAlias = serde_json::from_slice(&raw).map_err(cache_err)?;
                if sa.alias.kind == AliasKind::User {
                    table.remove(alias_id).map_err(cache_err)?;
                    true
                } else {
                    false
                }
            }
        }
    };
    w.commit().map_err(cache_err)?;
    Ok(removed)
}

fn list(db: &Database) -> Result<Vec<CachedTarget>, CacheError> {
    let txn = db.begin_read().map_err(cache_err)?;
    let aliases_tbl = txn.open_table(ALIASES).map_err(cache_err)?;
    let mut aliases_by: HashMap<String, Vec<ResolvedAlias>> = HashMap::new();
    for entry in aliases_tbl.iter().map_err(cache_err)? {
        let (_, v) = entry.map_err(cache_err)?;
        let sa: StoredAlias = serde_json::from_slice(v.value()).map_err(cache_err)?;
        aliases_by.entry(sa.target_id).or_default().push(sa.alias);
    }

    let targets = txn.open_table(TARGETS).map_err(cache_err)?;
    let mut out = Vec::new();
    for entry in targets.iter().map_err(cache_err)? {
        let (k, v) = entry.map_err(cache_err)?;
        let id_str = k.value().to_string();
        let st: StoredTarget = serde_json::from_slice(v.value()).map_err(cache_err)?;
        let id =
            Uuid::parse_str(&id_str).map_err(|e| CacheError::InvalidUuid(id_str.clone(), e))?;
        let mut aliases = aliases_by.remove(&id_str).unwrap_or_default();
        aliases.sort_by(|a, b| a.alias.cmp(&b.alias));
        out.push(st.into_target(id, aliases));
    }
    out.sort_by(|a, b| a.primary_designation.cmp(&b.primary_designation));
    Ok(out)
}

#[async_trait::async_trait]
impl Cache for RedbCache {
    async fn get_by_id(&self, id: Uuid) -> Result<Option<CachedTarget>, CacheError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || get_by_id(&db, id)).await.map_err(cache_err)?
    }

    async fn get_by_simbad_oid(&self, oid: i64) -> Result<Option<CachedTarget>, CacheError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || get_by_simbad_oid(&db, oid)).await.map_err(cache_err)?
    }

    async fn get_by_normalized(
        &self,
        normalized: &str,
    ) -> Result<Option<CachedTarget>, CacheError> {
        let db = self.db.clone();
        let normalized = normalized.to_owned();
        tokio::task::spawn_blocking(move || get_by_normalized(&db, &normalized))
            .await
            .map_err(cache_err)?
    }

    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, CacheError> {
        let db = self.db.clone();
        let query = query.to_owned();
        tokio::task::spawn_blocking(move || search(&db, &query, limit)).await.map_err(cache_err)?
    }

    async fn upsert(
        &self,
        identity: &ResolvedIdentity,
        namespace: &Uuid,
    ) -> Result<(Uuid, UpsertOutcome), CacheError> {
        let db = self.db.clone();
        let identity = identity.clone();
        let namespace = *namespace;
        tokio::task::spawn_blocking(move || upsert(&db, &identity, &namespace))
            .await
            .map_err(cache_err)?
    }

    async fn upsert_batch(
        &self,
        identities: &[ResolvedIdentity],
        namespace: &Uuid,
    ) -> Result<Vec<(Uuid, UpsertOutcome)>, CacheError> {
        let db = self.db.clone();
        let identities = identities.to_vec();
        let namespace = *namespace;
        let durability = self.bulk_durability;
        tokio::task::spawn_blocking(move || upsert_batch(&db, &identities, &namespace, durability))
            .await
            .map_err(cache_err)?
    }

    async fn add_user_alias(&self, target_id: Uuid, alias: &str) -> Result<bool, CacheError> {
        let db = self.db.clone();
        let alias = alias.to_owned();
        tokio::task::spawn_blocking(move || add_user_alias(&db, target_id, &alias))
            .await
            .map_err(cache_err)?
    }

    async fn remove_user_alias(&self, alias_id: &str) -> Result<bool, CacheError> {
        let db = self.db.clone();
        let alias_id = alias_id.to_owned();
        tokio::task::spawn_blocking(move || remove_user_alias(&db, &alias_id))
            .await
            .map_err(cache_err)?
    }

    async fn list(&self) -> Result<Vec<CachedTarget>, CacheError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || list(&db)).await.map_err(cache_err)?
    }
}

impl RedbCache {
    /// Force one fully durable (fsync'd) commit against this store, touching
    /// no data. Inherent (not on the `Cache` trait) because durability is a
    /// redb-specific, storage-engine concept, not a portable identity-store
    /// operation — callers need a concrete [`RedbCache`] (via
    /// [`Store::cache`]), not just `impl Cache`/`dyn Cache`.
    ///
    /// Call this once right after the last chunk of a
    /// [`BatchDurability::Eventual`] bulk load (see [`Store::open_with`]) —
    /// don't rely on the store closing naturally to persist those chunks, as
    /// it may stay open for the rest of the process's lifetime and a crash
    /// any time before it closes would lose everything since the last durable
    /// commit (see [`BatchDurability`] for the full crash-vs-clean-shutdown
    /// distinction). redb commits are cumulative, so this one fsync persists
    /// every prior `Eventual` `upsert_batch` commit too, not just this call.
    /// A no-op-but-safe-to-call if this store's bulk durability is
    /// [`BatchDurability::Durable`] (every write already fsyncs on its own).
    ///
    /// # Errors
    ///
    /// [`CacheError::Backend`] if the (empty) transaction fails to commit.
    pub async fn flush(&self) -> Result<(), CacheError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || flush_durable(&db)).await.map_err(cache_err)?
    }
}

// ── Queue sync operations ─────────────────────────────────────────────────────

fn enqueue(db: &Database, id: &str, query: &str) -> Result<(), QueueError> {
    let w = db.begin_write().map_err(queue_err)?;
    {
        let mut pending = w.open_table(PENDING).map_err(queue_err)?;
        if pending.get(id).map_err(queue_err)?.is_none() {
            let mut meta = w.open_table(META).map_err(queue_err)?;
            let seq = meta.get(PENDING_SEQ_KEY).map_err(queue_err)?.map_or(0, |g| g.value());
            meta.insert(PENDING_SEQ_KEY, seq + 1).map_err(queue_err)?;
            let sp = StoredPending {
                query: query.to_owned(),
                state: PendingState::Pending.as_wire().to_owned(),
                attempts: 0,
                target_id: None,
                seq,
            };
            let bytes = serde_json::to_vec(&sp).map_err(queue_err)?;
            pending.insert(id, bytes.as_slice()).map_err(queue_err)?;
        }
    }
    w.commit().map_err(queue_err)?;
    Ok(())
}

fn claim_pending(db: &Database, n: usize) -> Result<Vec<PendingItem>, QueueError> {
    let txn = db.begin_read().map_err(queue_err)?;
    let pending = txn.open_table(PENDING).map_err(queue_err)?;
    let mut rows: Vec<(String, StoredPending)> = Vec::new();
    for entry in pending.iter().map_err(queue_err)? {
        let (k, v) = entry.map_err(queue_err)?;
        let sp: StoredPending = serde_json::from_slice(v.value()).map_err(queue_err)?;
        if sp.state == PendingState::Pending.as_wire() {
            rows.push((k.value().to_string(), sp));
        }
    }
    rows.sort_by_key(|(_, sp)| sp.seq);
    rows.truncate(n);
    rows.into_iter().map(|(id, sp)| sp.into_item(id)).collect()
}

/// Read-modify-write one pending row (a no-op if the id is absent).
fn update_pending(
    db: &Database,
    id: &str,
    mutate: impl FnOnce(&mut StoredPending),
) -> Result<(), QueueError> {
    let w = db.begin_write().map_err(queue_err)?;
    {
        let mut pending = w.open_table(PENDING).map_err(queue_err)?;
        // Copy the row out so the read guard is dropped before the write borrow.
        let existing = pending.get(id).map_err(queue_err)?.map(|v| v.value().to_vec());
        if let Some(raw) = existing {
            let mut sp: StoredPending = serde_json::from_slice(&raw).map_err(queue_err)?;
            mutate(&mut sp);
            let bytes = serde_json::to_vec(&sp).map_err(queue_err)?;
            pending.insert(id, bytes.as_slice()).map_err(queue_err)?;
        }
    }
    w.commit().map_err(queue_err)?;
    Ok(())
}

fn get(db: &Database, id: &str) -> Result<Option<PendingItem>, QueueError> {
    let txn = db.begin_read().map_err(queue_err)?;
    let pending = txn.open_table(PENDING).map_err(queue_err)?;
    match pending.get(id).map_err(queue_err)? {
        None => Ok(None),
        Some(v) => {
            let sp: StoredPending = serde_json::from_slice(v.value()).map_err(queue_err)?;
            Ok(Some(sp.into_item(id.to_owned())?))
        }
    }
}

fn pending_count(db: &Database) -> Result<usize, QueueError> {
    let txn = db.begin_read().map_err(queue_err)?;
    let pending = txn.open_table(PENDING).map_err(queue_err)?;
    let mut count = 0usize;
    for entry in pending.iter().map_err(queue_err)? {
        let (_, v) = entry.map_err(queue_err)?;
        let sp: StoredPending = serde_json::from_slice(v.value()).map_err(queue_err)?;
        if sp.state == PendingState::Pending.as_wire() {
            count += 1;
        }
    }
    Ok(count)
}

#[async_trait::async_trait]
impl Queue for RedbQueue {
    async fn enqueue(&self, id: &str, query: &str) -> Result<(), QueueError> {
        let db = self.db.clone();
        let (id, query) = (id.to_owned(), query.to_owned());
        tokio::task::spawn_blocking(move || enqueue(&db, &id, &query)).await.map_err(queue_err)?
    }

    async fn claim_pending(&self, n: usize) -> Result<Vec<PendingItem>, QueueError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || claim_pending(&db, n)).await.map_err(queue_err)?
    }

    async fn mark_resolved(&self, id: &str, target_id: Uuid) -> Result<(), QueueError> {
        let db = self.db.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            update_pending(&db, &id, |sp| {
                PendingState::Resolved.as_wire().clone_into(&mut sp.state);
                sp.target_id = Some(target_id.to_string());
            })
        })
        .await
        .map_err(queue_err)?
    }

    async fn mark_unresolved(&self, id: &str) -> Result<(), QueueError> {
        let db = self.db.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            update_pending(&db, &id, |sp| {
                PendingState::Unresolved.as_wire().clone_into(&mut sp.state);
                sp.attempts += 1;
            })
        })
        .await
        .map_err(queue_err)?
    }

    async fn release(&self, id: &str) -> Result<(), QueueError> {
        let db = self.db.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            update_pending(&db, &id, |sp| {
                PendingState::Pending.as_wire().clone_into(&mut sp.state);
            })
        })
        .await
        .map_err(queue_err)?
    }

    async fn get(&self, id: &str) -> Result<Option<PendingItem>, QueueError> {
        let db = self.db.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || get(&db, &id)).await.map_err(queue_err)?
    }

    async fn pending_count(&self) -> Result<usize, QueueError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || pending_count(&db)).await.map_err(queue_err)?
    }
}

#[cfg(test)]
mod tests {
    //! Covers `remove_user_alias`, whose real-removal path needs an opaque
    //! alias id the public API never exposes — the dashmap backend tested this
    //! via internals too, so it lives here rather than in `tests/cache.rs`.
    use super::*;

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

    /// Recover an opaque alias id by its display form (test-only introspection).
    fn alias_id_of(db: &Database, display: &str) -> String {
        let txn = db.begin_read().unwrap();
        let table = txn.open_table(ALIASES).unwrap();
        for entry in table.iter().unwrap() {
            let (k, v) = entry.unwrap();
            let sa: StoredAlias = serde_json::from_slice(v.value()).unwrap();
            if sa.alias.alias == display {
                return k.value().to_string();
            }
        }
        panic!("alias {display:?} not found");
    }

    /// Serializes every test in this module that calls `upsert`/`upsert_batch`
    /// against [`UPSERT_WRITE_TXN_COUNT`]/[`FULL_TABLE_SCAN_COUNT`] readers —
    /// `cargo test` runs tests on concurrent threads within one binary, and
    /// those counters are process-wide statics, so an unguarded interleaving
    /// would flake the counts. `tokio::sync::Mutex` (not `std::sync::Mutex`)
    /// because the guard is held across `.await` points.
    static TXN_COUNT_GUARD: std::sync::LazyLock<tokio::sync::Mutex<()>> =
        std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

    #[tokio::test]
    async fn remove_user_alias_only_removes_user_kind() {
        let _guard = TXN_COUNT_GUARD.lock().await;
        let store = Store::in_memory().unwrap();
        let cache = store.cache();
        let ns = crate::identity::namespace("redb.tests");
        let (id, _) = cache.upsert(&m31(), &ns).await.unwrap();
        cache.add_user_alias(id, "M31 favorite").await.unwrap();

        let user_alias_id = alias_id_of(&store.db, "M31 favorite");
        assert!(cache.remove_user_alias(&user_alias_id).await.unwrap());
        assert_eq!(cache.get_by_id(id).await.unwrap().unwrap().aliases.len(), 3);

        // A designation alias's id must not be removable via remove_user_alias.
        let designation_id = alias_id_of(&store.db, "M 31");
        assert!(!cache.remove_user_alias(&designation_id).await.unwrap());
        assert_eq!(cache.get_by_id(id).await.unwrap().unwrap().aliases.len(), 3);
    }

    /// `identities[i]` is a distinct target: distinct oid, designation, and
    /// aliases, so batching them never collides on dedup.
    fn distinct_identities(n: usize) -> Vec<ResolvedIdentity> {
        (0..n)
            .map(|i| {
                let idx = u32::try_from(i).expect("test batch sizes fit in u32");
                let designation = format!("Batch Target {i}");
                ResolvedIdentity {
                    simbad_oid: Some(9_000_000 + i64::from(idx)),
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
            .collect()
    }

    #[tokio::test]
    async fn upsert_batch_uses_one_write_transaction_regardless_of_batch_size() {
        let _guard = TXN_COUNT_GUARD.lock().await;
        UPSERT_WRITE_TXN_COUNT.store(0, std::sync::atomic::Ordering::Relaxed);

        let store = Store::in_memory().unwrap();
        let cache = store.cache();
        let ns = crate::identity::namespace("redb.tests.batch-txn-count");
        let identities = distinct_identities(25);

        let results = cache.upsert_batch(&identities, &ns).await.unwrap();
        assert_eq!(results.len(), 25);
        assert_eq!(
            UPSERT_WRITE_TXN_COUNT.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "one upsert_batch call of 25 identities must open exactly one write transaction"
        );

        UPSERT_WRITE_TXN_COUNT.store(0, std::sync::atomic::Ordering::Relaxed);
        for identity in &identities {
            cache.upsert(identity, &ns).await.unwrap();
        }
        assert_eq!(
            UPSERT_WRITE_TXN_COUNT.load(std::sync::atomic::Ordering::Relaxed),
            25,
            "25 sequential upsert calls must open one write transaction each"
        );
    }

    /// Regression guard for nightwatch-astro/alm#695's O(n²) defect:
    /// `upsert_within`'s old oid dedup *and* its `rewrite_aliases` call both
    /// re-scanned their whole (growing) table once per entry. `upsert_batch`
    /// must instead do exactly one full scan of each (`build_oid_index` +
    /// `build_alias_index`) per batch call, regardless of batch size —
    /// asserted here directly via the scan counter rather than by timing,
    /// since a count assertion can't flake on a slow CI runner.
    #[tokio::test]
    async fn upsert_batch_does_one_full_scan_per_table_regardless_of_batch_size() {
        let _guard = TXN_COUNT_GUARD.lock().await;
        FULL_TABLE_SCAN_COUNT.store(0, std::sync::atomic::Ordering::Relaxed);

        let store = Store::in_memory().unwrap();
        let cache = store.cache();
        let ns = crate::identity::namespace("redb.tests.batch-scan-count");
        let identities = distinct_identities(50);

        cache.upsert_batch(&identities, &ns).await.unwrap();
        assert_eq!(
            FULL_TABLE_SCAN_COUNT.load(std::sync::atomic::Ordering::Relaxed),
            2,
            "one upsert_batch call of 50 identities must do exactly one full-table oid scan \
             (build_oid_index) plus one full-table alias scan (build_alias_index), total 2"
        );

        FULL_TABLE_SCAN_COUNT.store(0, std::sync::atomic::Ordering::Relaxed);
        for identity in &identities {
            cache.upsert(identity, &ns).await.unwrap();
        }
        assert_eq!(
            FULL_TABLE_SCAN_COUNT.load(std::sync::atomic::Ordering::Relaxed),
            100,
            "50 sequential upsert calls must each do their own oid scan + alias scan, total 100"
        );
    }

    /// `upsert_batch` must apply the exact same per-entry dedup + precedence
    /// rules as a sequential loop of single `upsert` calls — including
    /// `SkippedUserOverride`, and including dedup against an earlier entry of
    /// the *same* batch (a batch may legitimately re-touch a target, e.g. a
    /// seed re-warm after a user override was already recorded).
    #[tokio::test]
    async fn upsert_batch_matches_sequential_upsert_semantics() {
        // This test doesn't read UPSERT_WRITE_TXN_COUNT, but it does call
        // upsert/upsert_batch, which increments that shared static — hold the
        // same guard so it can't skew a concurrently-running txn-count test.
        let _guard = TXN_COUNT_GUARD.lock().await;
        let ns = crate::identity::namespace("redb.tests.batch-semantics");

        // A user override recorded first must survive a batch that later
        // tries to overwrite the same oid with a lower-precedence source.
        let store = Store::in_memory().unwrap();
        let cache = store.cache();
        let (override_id, _) = cache.upsert(&m31(), &ns).await.unwrap(); // TargetSource::Resolved
        let mut user_override = m31();
        user_override.source = TargetSource::UserOverride;
        "My Andromeda".clone_into(&mut user_override.primary_designation);
        cache.upsert(&user_override, &ns).await.unwrap();

        let mut reseed = m31(); // TargetSource::Resolved, same oid: must be skipped
        "WRONG".clone_into(&mut reseed.primary_designation);
        let mut other = distinct_identities(1).remove(0);
        let batch = vec![other.clone(), reseed.clone()];
        let outcomes = cache.upsert_batch(&batch, &ns).await.unwrap();
        assert_eq!(outcomes[1].0, override_id);
        assert_eq!(outcomes[1].1, UpsertOutcome::SkippedUserOverride);
        let got = cache.get_by_id(override_id).await.unwrap().unwrap();
        assert_eq!(got.primary_designation, "My Andromeda", "user override stays sticky");

        // A batch that upserts the same never-before-seen designation twice
        // (no oid, so dedup falls back to the derived id) must dedup the
        // second entry against the *first entry of the same batch*, not just
        // against already-committed rows.
        other.simbad_oid = None;
        "Repeat Target".clone_into(&mut other.primary_designation);
        let mut again = other.clone();
        again.dec_deg = 12.5;
        let batch2 = vec![other.clone(), again];
        let outcomes2 = cache.upsert_batch(&batch2, &ns).await.unwrap();
        assert_eq!(outcomes2[0].1, UpsertOutcome::Inserted, "first-ever row for this designation");
        assert_eq!(outcomes2[1].1, UpsertOutcome::Updated, "dedups against entry 0 of this batch");
        assert_eq!(outcomes2[0].0, outcomes2[1].0, "same derived id, same row");
        let got2 = cache.get_by_id(outcomes2[1].0).await.unwrap().unwrap();
        assert!((got2.dec_deg - 12.5).abs() < f64::EPSILON, "batch entry 1's value won");
    }

    /// Proves the cache-compatibility mechanism for the galdim fields: a
    /// `targets` row written by a pre-galdim build has no
    /// `galdim_majaxis_arcmin`/`galdim_minaxis_arcmin`/`galdim_angle_deg` keys
    /// in its JSON blob at all. `#[serde(default)]` on those `StoredTarget`
    /// fields (the same mechanism `v_mag` already relies on, see its own
    /// doc comment) must let this deserialize as `None` rather than failing to
    /// load or silently corrupting the row. This writes the OLD shape directly
    /// into the `targets` table, bypassing `upsert`/`StoredTarget` entirely, so
    /// the test can't accidentally exercise the new (already-compatible) write
    /// path instead of proving the old-row read path.
    #[tokio::test]
    async fn galdim_pre_upgrade_row_deserializes_with_none() {
        let _guard = TXN_COUNT_GUARD.lock().await;
        let store = Store::in_memory().unwrap();
        let id = Uuid::new_v4();

        let pre_upgrade_row = serde_json::json!({
            "simbad_oid": 1_575_544,
            "primary_designation": "M 31",
            "common_name": "Andromeda Galaxy",
            "object_type": "galaxy",
            "otype_raw": "G",
            "ra_deg": 10.684_708,
            "dec_deg": 41.268_75,
            "v_mag": 3.44,
            "source": "resolved",
            "resolved_at": "2026-07-11T00:00:00Z",
        });
        let bytes = serde_json::to_vec(&pre_upgrade_row).unwrap();
        let w = store.db.begin_write().unwrap();
        {
            let mut table = w.open_table(TARGETS).unwrap();
            table.insert(id.to_string().as_str(), bytes.as_slice()).unwrap();
        }
        w.commit().unwrap();

        let target =
            store.cache().get_by_id(id).await.unwrap().expect("pre-upgrade row still loads");
        assert_eq!(target.primary_designation, "M 31");
        assert_eq!(target.v_mag, Some(3.44), "a field that existed pre-upgrade is unaffected");
        assert_eq!(target.galdim_majaxis_arcmin, None, "absent key deserializes as None, not 0.0");
        assert_eq!(target.galdim_minaxis_arcmin, None);
        assert_eq!(target.galdim_angle_deg, None);
    }
}
