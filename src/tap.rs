// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! SIMBAD TAP client: maps SIMBAD responses to canonical target identity.
//!
//! Talks to the SIMBAD TAP `sim-tap/sync` endpoint (ADQL, TSV) over HTTPS via
//! an async `reqwest` client, with polite usage (configurable timeout + an
//! identifying `User-Agent`). A query with no `basic` row returns
//! [`ResolveError::NotFound`]; a query that maps to several distinct physical
//! objects returns [`ResolveError::Ambiguous`].
//!
//! # Query shape
//!
//! Name resolution ([`Resolver::resolve`]) is two ADQL round-trips against
//! the TAP sync endpoint:
//!
//! 1. `basic ⋈ ident`, `LEFT OUTER JOIN allfluxes`, to find the object(s) whose
//!    `ident.id` matches the query (verbatim and whitespace-collapsed) and pull
//!    `oid, main_id, ra, dec, otype_txt, V` (V-band magnitude, `NULL` when the
//!    object has no V photometry) plus `galdim_majaxis`, `galdim_minaxis`
//!    (arcminutes) and `galdim_angle` (degrees), `NULL` for a point source.
//! 2. `ident` for the winning `oid` to pull the full alias set, where
//!    `NAME …` rows are curated common names.
//!
//! Position resolution ([`PositionResolver::resolve_position`]) is a single
//! ADQL cone search (`CONTAINS`/`CIRCLE`/`DISTANCE`) ordered nearest-first,
//! without the alias round-trip: each hit's only alias is its primary
//! designation.
//!
//! ## Gotchas
//!
//! - **`format=tsv`**: string columns are double-quoted; numeric columns are
//!   not. [`crate::wire`] strips quotes and collapses internal
//!   whitespace runs — SIMBAD emits space-padded ids like `"M   31"`.
//! - **No `REPLACE` UDF**: SIMBAD TAP has no string UDFs; matching the query
//!   both verbatim and whitespace-collapsed compensates for `ident.id`'s own
//!   internal-whitespace collapsing.
//! - The first TSV line is the column header and is stripped before parsing.

use std::time::Duration;

use async_trait::async_trait;

use crate::{
    map_otype, wire, AliasKind, PositionMatch, PositionResolver, ResolveError, ResolvedAlias,
    ResolvedIdentity, Resolver, SimbadConfig, TargetSource,
};

/// Hard cap on a single TAP response body. A resolve query targets one object
/// plus its alias list (or a bounded cone-search page); even rich objects are
/// well under 1 MB, so 8 MiB is a generous bound that still prevents memory
/// exhaustion from a hostile/misbehaving endpoint.
const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;

/// Live SIMBAD TAP resolver: implements [`Resolver`] and [`PositionResolver`].
///
/// Queries the real SIMBAD TAP endpoint, so this example is `no_run` — it
/// needs network access to complete.
///
/// ```no_run
/// use simbad_resolver::{Resolver, TapResolver};
///
/// # async fn demo() -> Result<(), simbad_resolver::ResolveError> {
/// let resolver = TapResolver::with_defaults()?;
/// let identity = resolver.resolve("M 31").await?;
/// println!("{} @ ({}, {})", identity.primary_designation, identity.ra_deg, identity.dec_deg);
/// # Ok(()) }
/// ```
#[derive(Debug)]
pub struct TapResolver {
    client: reqwest::Client,
    endpoint: url::Url,
    timeout: Duration,
}

impl TapResolver {
    /// Construct a resolver from a [`SimbadConfig`].
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError::Network`] if `config.endpoint` is not a valid
    /// URL, or if the underlying `reqwest` client cannot be built (e.g. TLS
    /// backend init failure).
    ///
    /// Only builds the HTTP client and parses the endpoint URL — no request is
    /// sent, so this is runnable.
    ///
    /// ```
    /// use simbad_resolver::{SimbadConfig, TapResolver};
    ///
    /// let config =
    ///     SimbadConfig { endpoint: "https://example.test/tap".to_owned(), ..SimbadConfig::default() };
    /// let resolver = TapResolver::new(&config)?;
    /// # let _ = resolver;
    /// # Ok::<(), simbad_resolver::ResolveError>(())
    /// ```
    pub fn new(config: &SimbadConfig) -> Result<Self, ResolveError> {
        let endpoint = url::Url::parse(&config.endpoint).map_err(|e| {
            ResolveError::Network(format!("invalid TAP endpoint {:?}: {e}", config.endpoint))
        })?;
        let client = reqwest::Client::builder()
            .user_agent(config.user_agent.clone())
            .timeout(config.timeout)
            .build()
            .map_err(|e| ResolveError::Network(e.to_string()))?;
        Ok(Self { client, endpoint, timeout: config.timeout })
    }

    /// Convenience constructor using [`SimbadConfig::default`].
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError::Network`] if the client cannot be built.
    ///
    /// ```
    /// use simbad_resolver::TapResolver;
    ///
    /// let resolver = TapResolver::with_defaults()?;
    /// # let _ = resolver;
    /// # Ok::<(), simbad_resolver::ResolveError>(())
    /// ```
    pub fn with_defaults() -> Result<Self, ResolveError> {
        Self::new(&SimbadConfig::default())
    }

    /// Run one ADQL query against the TAP sync endpoint, returning the raw
    /// response body (`format=tsv`) after bounding and error-body checks.
    ///
    /// The response is bounded to [`MAX_RESPONSE_BYTES`] (an advertised
    /// `Content-Length` check plus a streamed running total) so a
    /// misbehaving/hostile endpoint can't exhaust memory via an unbounded
    /// read. A TAP error can come back as a VOTable/text body under HTTP 200;
    /// that shape is detected and reported as [`ResolveError::Parse`].
    async fn tap_query(&self, adql: &str) -> Result<String, ResolveError> {
        tracing::debug!(query = adql, "SIMBAD TAP request");
        let resp = self
            .client
            .get(self.endpoint.clone())
            .query(&[("request", "doQuery"), ("lang", "ADQL"), ("format", "tsv"), ("query", adql)])
            .send()
            .await
            .map_err(|e| classify_reqwest(&e, self.timeout))?;
        let mut resp = resp.error_for_status().map_err(|e| classify_reqwest(&e, self.timeout))?;

        if let Some(len) = resp.content_length() {
            if len > MAX_RESPONSE_BYTES {
                return Err(ResolveError::Parse(format!(
                    "SIMBAD response too large ({len} bytes > {MAX_RESPONSE_BYTES} cap)"
                )));
            }
        }
        let mut buf: Vec<u8> = Vec::new();
        let mut total: u64 = 0;
        while let Some(chunk) =
            resp.chunk().await.map_err(|e| classify_reqwest(&e, self.timeout))?
        {
            total += chunk.len() as u64;
            if total > MAX_RESPONSE_BYTES {
                return Err(ResolveError::Parse(format!(
                    "SIMBAD response exceeded {MAX_RESPONSE_BYTES} byte cap"
                )));
            }
            buf.extend_from_slice(&chunk);
        }
        let body = String::from_utf8_lossy(&buf).into_owned();

        if is_votable_error(&body) {
            return Err(ResolveError::Parse("SIMBAD returned a VOTable error".to_owned()));
        }
        Ok(body)
    }

    /// Find the distinct `basic` rows whose `ident.id` matches `query`
    /// (verbatim or whitespace-collapsed).
    async fn find_objects(&self, query: &str) -> Result<Vec<BasicRow>, ResolveError> {
        // Match on the verbatim query and its single-space-collapsed form,
        // SQL-quoting each literal; `ident.id` matching collapses internal
        // whitespace itself, so a padded stored id still matches.
        let collapsed = wire::collapse_spaces(query);
        let mut id_forms: Vec<String> = vec![query.to_owned()];
        if collapsed != query {
            id_forms.push(collapsed);
        }
        let list = id_forms
            .iter()
            .map(|id| format!("'{}'", id.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(", ");

        // `allfluxes` is 1:1 with `basic` (LEFT OUTER JOIN → V is NULL when the
        // object has no V photometry, so the object still resolves). `V` is
        // uppercase: `allfluxes` is SIMBAD's one case-sensitive table.
        // `galdim_majaxis`/`galdim_minaxis`/`galdim_angle` live directly on
        // `basic` and are NULL for a point source (e.g. a star) — no join needed.
        let q = format!(
            "SELECT DISTINCT b.oid, b.main_id, b.ra, b.dec, b.otype_txt, f.V, \
             b.galdim_majaxis, b.galdim_minaxis, b.galdim_angle \
             FROM basic AS b JOIN ident AS i ON i.oidref = b.oid \
             LEFT OUTER JOIN allfluxes AS f ON f.oidref = b.oid \
             WHERE i.id IN ({list}) AND b.ra IS NOT NULL AND b.dec IS NOT NULL"
        );
        let body = self.tap_query(&q).await?;
        Ok(parse_basic_rows(&body))
    }

    /// Pull the alias set (designations + `NAME …` common names) for one oid.
    async fn fetch_aliases(
        &self,
        oid: i64,
    ) -> Result<(Vec<ResolvedAlias>, Option<String>), ResolveError> {
        let q = format!("SELECT i.id FROM ident AS i WHERE i.oidref = {oid}");
        let body = self.tap_query(&q).await?;
        Ok(assemble_aliases(&body))
    }
}

#[async_trait]
impl Resolver for TapResolver {
    async fn resolve(&self, query: &str) -> Result<ResolvedIdentity, ResolveError> {
        let query = query.trim();
        if query.is_empty() {
            return Err(ResolveError::NotFound(String::new()));
        }

        let objects = self.find_objects(query).await?;
        match objects.len() {
            0 => Err(ResolveError::NotFound(query.to_owned())),
            1 => {
                let (oid, main_id, ra_deg, dec_deg, otype_raw, v_mag, majaxis, minaxis, angle) =
                    objects.into_iter().next().expect("len == 1 checked above");
                let (aliases, common_name) = self.fetch_aliases(oid).await?;
                Ok(assemble_identity(
                    oid,
                    &main_id,
                    ra_deg,
                    dec_deg,
                    &otype_raw,
                    v_mag,
                    majaxis,
                    minaxis,
                    angle,
                    aliases,
                    common_name,
                ))
            }
            n => Err(ResolveError::Ambiguous { query: query.to_owned(), count: n }),
        }
    }
}

#[async_trait]
impl PositionResolver for TapResolver {
    async fn resolve_position(
        &self,
        ra_deg: f64,
        dec_deg: f64,
        radius_deg: f64,
        limit: usize,
    ) -> Result<Vec<PositionMatch>, ResolveError> {
        let q = format!(
            "SELECT TOP {limit} b.oid, b.main_id, b.ra, b.dec, b.otype_txt, f.V, \
             b.galdim_majaxis, b.galdim_minaxis, b.galdim_angle, \
             DISTANCE(POINT('ICRS', b.ra, b.dec), POINT('ICRS', {ra_deg}, {dec_deg})) AS dist \
             FROM basic AS b \
             LEFT OUTER JOIN allfluxes AS f ON f.oidref = b.oid \
             WHERE CONTAINS(POINT('ICRS', b.ra, b.dec), CIRCLE('ICRS', {ra_deg}, {dec_deg}, {radius_deg})) = 1 \
             AND b.ra IS NOT NULL \
             ORDER BY dist ASC"
        );
        let body = self.tap_query(&q).await?;
        Ok(parse_position_rows(&body)
            .into_iter()
            .map(|(oid, main_id, ra, dec, otype_raw, v_mag, majaxis, minaxis, angle, dist)| {
                PositionMatch {
                    // No second alias round-trip for a cone hit: `assemble_identity`
                    // still includes the primary designation in the alias set.
                    identity: assemble_identity(
                        oid,
                        &main_id,
                        ra,
                        dec,
                        &otype_raw,
                        v_mag,
                        majaxis,
                        minaxis,
                        angle,
                        Vec::new(),
                        None,
                    ),
                    separation_deg: dist,
                }
            })
            .collect())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// A parsed `basic ⋈ allfluxes` row: `(oid, main_id, ra, dec, otype, v_mag,
/// galdim_majaxis, galdim_minaxis, galdim_angle)`.
type BasicRow = (i64, String, f64, f64, String, Option<f64>, Option<f64>, Option<f64>, Option<i16>);
/// A parsed cone-search row: a [`BasicRow`] plus the trailing angular `dist`.
type PositionRow =
    (i64, String, f64, f64, String, Option<f64>, Option<f64>, Option<f64>, Option<i16>, f64);

/// Classify a `reqwest` error into the right [`ResolveError`] so callers can
/// degrade to seed+cache on transport failure / timeout.
fn classify_reqwest(e: &reqwest::Error, timeout: Duration) -> ResolveError {
    if e.is_timeout() {
        ResolveError::Timeout(timeout.as_secs())
    } else {
        ResolveError::Network(e.to_string())
    }
}

/// Detect a TAP error returned as a VOTable/text body under HTTP 200 (rather
/// than a non-2xx status, which `error_for_status` already classifies).
fn is_votable_error(body: &str) -> bool {
    body.contains("<VOTABLE") && body.contains("ERROR")
}

/// Split a raw TAP TSV response body into data lines, stripping the header
/// row (always present, even for a zero-row result).
fn tsv_data_lines(body: &str) -> impl Iterator<Item = &str> {
    body.lines().skip(1).filter(|l| !l.trim().is_empty())
}

/// Parse all `basic ⋈ ident` rows from a raw TAP TSV body (header stripped).
/// A malformed row (bad numeric field, out-of-range coordinate) is silently
/// dropped rather than aborting the whole result set.
fn parse_basic_rows(body: &str) -> Vec<BasicRow> {
    tsv_data_lines(body).filter_map(wire::parse_basic_row).collect()
}

/// Parse one cone-search TSV row (`oid, main_id, ra, dec, otype_txt, V,
/// galdim_majaxis, galdim_minaxis, galdim_angle, dist`).
///
/// Reuses [`wire::parse_basic_row`] for the first nine columns so the RA/Dec
/// range validation, V-magnitude, galdim, and quoting rules aren't duplicated;
/// only the trailing `dist` column is parsed here.
fn parse_position_row(line: &str) -> Option<PositionRow> {
    let fields: Vec<&str> = line.split('\t').collect();
    if fields.len() < 10 {
        return None;
    }
    let (oid, main_id, ra, dec, otype, v_mag, majaxis, minaxis, angle) =
        wire::parse_basic_row(&fields[..9].join("\t"))?;
    let dist: f64 = wire::unquote(fields[9]).parse().ok()?;
    if !dist.is_finite() || dist < 0.0 {
        return None;
    }
    Some((oid, main_id, ra, dec, otype, v_mag, majaxis, minaxis, angle, dist))
}

/// Parse all cone-search rows from a raw TAP TSV body (header stripped).
fn parse_position_rows(body: &str) -> Vec<PositionRow> {
    tsv_data_lines(body).filter_map(parse_position_row).collect()
}

/// Parse a raw `ident` TSV body (single `id` column, header stripped) into
/// the alias set plus curated common name for one `oid`. A `NAME …` row is a
/// SIMBAD curated common name; every other row is a catalog designation.
fn assemble_aliases(body: &str) -> (Vec<ResolvedAlias>, Option<String>) {
    let mut aliases: Vec<ResolvedAlias> = Vec::new();
    let mut common_name: Option<String> = None;
    for line in tsv_data_lines(body) {
        // Single-column query: the whole line is the (quoted) id.
        let id_raw = wire::unquote(line);
        if id_raw.is_empty() {
            continue;
        }
        if let Some(name) = id_raw.strip_prefix("NAME ") {
            let name = name.trim();
            if common_name.is_none() {
                common_name = Some(name.to_owned());
            }
            push_unique(&mut aliases, name, AliasKind::CommonName);
        } else {
            push_unique(&mut aliases, &wire::collapse_spaces(&id_raw), AliasKind::Designation);
        }
    }
    (aliases, common_name)
}

/// Build the canonical [`ResolvedIdentity`] from a parsed `basic` row plus its
/// alias set. The returned alias set always includes the primary designation
/// as an [`AliasKind::Designation`] entry, even when the alias round-trip
/// didn't return it, or, for a cone-search hit, wasn't run at all.
#[allow(clippy::too_many_arguments)]
fn assemble_identity(
    oid: i64,
    main_id: &str,
    ra_deg: f64,
    dec_deg: f64,
    otype_raw: &str,
    v_mag: Option<f64>,
    galdim_majaxis_arcmin: Option<f64>,
    galdim_minaxis_arcmin: Option<f64>,
    galdim_angle_deg: Option<i16>,
    mut aliases: Vec<ResolvedAlias>,
    common_name: Option<String>,
) -> ResolvedIdentity {
    let primary_designation = wire::collapse_spaces(main_id);
    push_unique(&mut aliases, &primary_designation, AliasKind::Designation);
    ResolvedIdentity {
        simbad_oid: Some(oid),
        primary_designation,
        common_name,
        object_type: map_otype(otype_raw),
        otype_raw: otype_raw.to_owned(),
        ra_deg,
        dec_deg,
        v_mag,
        galdim_majaxis_arcmin,
        galdim_minaxis_arcmin,
        galdim_angle_deg,
        aliases,
        source: TargetSource::Resolved,
    }
}

/// Append `alias` to `out` unless an equal display form is already present.
fn push_unique(out: &mut Vec<ResolvedAlias>, alias: &str, kind: AliasKind) {
    if alias.is_empty() || out.iter().any(|a| a.alias == alias) {
        return;
    }
    out.push(ResolvedAlias::new(alias, kind));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ObjectType;

    const M31_BASIC_TSV: &str =
        "oid\tmain_id\tra\tdec\totype_txt\tV\tgaldim_majaxis\tgaldim_minaxis\tgaldim_angle\n\
        1575544\t\"M  31\"\t10.6847083\t41.26875\t\"G\"\t3.44\t199.53\t70.79\t35\n";

    /// M 57: a near-circular planetary nebula, equal major/minor axes.
    const M57_BASIC_TSV: &str =
        "oid\tmain_id\tra\tdec\totype_txt\tV\tgaldim_majaxis\tgaldim_minaxis\tgaldim_angle\n\
        1560141\t\"M  57\"\t283.3963\t33.0292\t\"PN\"\t8.79\t1.153\t1.153\t90\n";

    /// Vega: a point source, no galdim measurement at all → NULL/empty columns.
    const VEGA_BASIC_TSV: &str =
        "oid\tmain_id\tra\tdec\totype_txt\tV\tgaldim_majaxis\tgaldim_minaxis\tgaldim_angle\n\
        3672809\t\"* alf Lyr\"\t279.2347\t38.7837\t\"*\"\t0.03\t\t\t\n";

    const AMBIGUOUS_BASIC_TSV: &str =
        "oid\tmain_id\tra\tdec\totype_txt\tV\tgaldim_majaxis\tgaldim_minaxis\tgaldim_angle\n\
        1575544\t\"M  31\"\t10.6847083\t41.26875\t\"G\"\t3.44\t199.53\t70.79\t35\n\
        999999\t\"Some Other\"\t11.0\t42.0\t\"G\"\t\t\t\t\n";

    const EMPTY_BASIC_TSV: &str =
        "oid\tmain_id\tra\tdec\totype_txt\tV\tgaldim_majaxis\tgaldim_minaxis\tgaldim_angle\n";

    const M31_ALIAS_TSV: &str = "id\n\
        \"M   31\"\n\
        \"NGC  224\"\n\
        \"NAME Andromeda Galaxy\"\n\
        \"NAME  Andromeda Nebula\"\n";

    const M31_POSITION_TSV: &str =
        "oid\tmain_id\tra\tdec\totype_txt\tV\tgaldim_majaxis\tgaldim_minaxis\tgaldim_angle\tdist\n\
        1575544\t\"M  31\"\t10.6847083\t41.26875\t\"G\"\t3.44\t199.53\t70.79\t35\t0.001\n\
        222\t\"NGC 206\"\t10.9\t40.7\t\"OpC\"\t\t\t\t\t0.6\n";

    const VOTABLE_ERROR_BODY: &str = "<VOTABLE version=\"1.4\"><RESOURCE type=\"results\">\
        <INFO name=\"QUERY_STATUS\" value=\"ERROR\">syntax error</INFO></RESOURCE></VOTABLE>";

    #[test]
    fn parse_basic_rows_extracts_one_object() {
        let rows = parse_basic_rows(M31_BASIC_TSV);
        assert_eq!(rows.len(), 1);
        let (oid, main_id, ra, dec, otype, v_mag, majaxis, minaxis, angle) = &rows[0];
        assert_eq!(*oid, 1_575_544);
        assert_eq!(main_id, "M  31");
        assert!((ra - 10.684_708_3).abs() < 1e-6);
        assert!((dec - 41.268_75).abs() < 1e-6);
        assert_eq!(otype, "G");
        assert_eq!(map_otype(otype), ObjectType::Galaxy);
        assert_eq!(*v_mag, Some(3.44));
        assert_eq!(*majaxis, Some(199.53));
        assert_eq!(*minaxis, Some(70.79));
        assert_eq!(*angle, Some(35));
    }

    #[test]
    fn parse_basic_rows_equal_axes_object() {
        let rows = parse_basic_rows(M57_BASIC_TSV);
        assert_eq!(rows.len(), 1);
        let (.., majaxis, minaxis, angle) = &rows[0];
        assert_eq!(*majaxis, Some(1.153));
        assert_eq!(*minaxis, Some(1.153));
        assert_eq!(*angle, Some(90));
    }

    #[test]
    fn parse_basic_rows_point_source_has_no_galdim() {
        // A point source (Vega) must read back None, never 0.0/0 — SIMBAD
        // genuinely has no angular-size measurement for it, distinct from a
        // measured-small value.
        let rows = parse_basic_rows(VEGA_BASIC_TSV);
        assert_eq!(rows.len(), 1);
        let (.., majaxis, minaxis, angle) = &rows[0];
        assert_eq!(*majaxis, None);
        assert_eq!(*minaxis, None);
        assert_eq!(*angle, None);
    }

    #[test]
    fn parse_basic_rows_zero_rows_is_not_found_precondition() {
        assert!(parse_basic_rows(EMPTY_BASIC_TSV).is_empty());
    }

    #[test]
    fn parse_basic_rows_multiple_rows_is_ambiguous_precondition() {
        assert_eq!(parse_basic_rows(AMBIGUOUS_BASIC_TSV).len(), 2);
    }

    #[test]
    fn assemble_aliases_splits_names_and_designations() {
        let (aliases, common_name) = assemble_aliases(M31_ALIAS_TSV);
        assert_eq!(common_name.as_deref(), Some("Andromeda Galaxy"));
        assert!(aliases.iter().any(|a| a.alias == "M 31" && a.kind == AliasKind::Designation));
        assert!(aliases.iter().any(|a| a.alias == "NGC 224" && a.kind == AliasKind::Designation));
        assert!(aliases
            .iter()
            .any(|a| a.alias == "Andromeda Galaxy" && a.kind == AliasKind::CommonName));
        assert!(aliases
            .iter()
            .any(|a| a.alias == "Andromeda Nebula" && a.kind == AliasKind::CommonName));
    }

    #[test]
    fn assemble_identity_builds_full_identity_and_dedupes_primary_alias() {
        let (aliases, common_name) = assemble_aliases(M31_ALIAS_TSV);
        let identity = assemble_identity(
            1_575_544,
            "M  31",
            10.684_708_3,
            41.268_75,
            "G",
            Some(3.44),
            Some(199.53),
            Some(70.79),
            Some(35),
            aliases,
            common_name,
        );
        assert_eq!(identity.simbad_oid, Some(1_575_544));
        assert_eq!(identity.primary_designation, "M 31");
        assert_eq!(identity.object_type, ObjectType::Galaxy);
        assert_eq!(identity.otype_raw, "G");
        assert_eq!(identity.v_mag, Some(3.44));
        assert_eq!(identity.galdim_majaxis_arcmin, Some(199.53));
        assert_eq!(identity.galdim_minaxis_arcmin, Some(70.79));
        assert_eq!(identity.galdim_angle_deg, Some(35));
        assert_eq!(identity.common_name.as_deref(), Some("Andromeda Galaxy"));
        assert_eq!(identity.source, TargetSource::Resolved);
        // "M 31" is already present from the alias round-trip (as "M   31"
        // collapsed); assemble_identity must not duplicate it.
        assert_eq!(identity.aliases.iter().filter(|a| a.alias == "M 31").count(), 1);
    }

    #[test]
    fn assemble_identity_adds_primary_designation_when_alias_list_is_empty() {
        // Mirrors a cone-search hit: no alias round-trip, so the only alias is
        // the primary designation.
        let identity = assemble_identity(
            42,
            "NGC 206",
            10.9,
            40.7,
            "OpC",
            None,
            None,
            None,
            None,
            Vec::new(),
            None,
        );
        assert_eq!(identity.aliases.len(), 1);
        assert_eq!(identity.aliases[0].alias, "NGC 206");
        assert_eq!(identity.aliases[0].kind, AliasKind::Designation);
        assert_eq!(identity.object_type, ObjectType::OpenCluster);
        assert_eq!(identity.v_mag, None);
        assert_eq!(identity.galdim_majaxis_arcmin, None);
        assert_eq!(identity.galdim_minaxis_arcmin, None);
        assert_eq!(identity.galdim_angle_deg, None);
    }

    #[test]
    fn parse_position_rows_parses_distance_column_in_order() {
        let rows = parse_position_rows(M31_POSITION_TSV);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, 1_575_544);
        assert_eq!(rows[0].5, Some(3.44));
        assert_eq!(rows[0].6, Some(199.53), "M 31 major axis carried through the cone search");
        assert_eq!(rows[0].7, Some(70.79));
        assert_eq!(rows[0].8, Some(35));
        assert!((rows[0].9 - 0.001).abs() < 1e-9);
        assert_eq!(rows[1].0, 222);
        assert_eq!(rows[1].5, None, "NGC 206 has no V mag → empty column → None");
        assert_eq!(rows[1].6, None, "NGC 206 has no galdim measurement → None");
        assert!((rows[1].9 - 0.6).abs() < 1e-9);
    }

    #[test]
    fn is_votable_error_detects_error_body_only() {
        assert!(is_votable_error(VOTABLE_ERROR_BODY));
        assert!(!is_votable_error(M31_BASIC_TSV));
    }

    #[test]
    fn push_unique_dedupes_by_display() {
        let mut v = Vec::new();
        push_unique(&mut v, "M 31", AliasKind::Designation);
        push_unique(&mut v, "M 31", AliasKind::Designation);
        push_unique(&mut v, "", AliasKind::Designation);
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn resolver_builds_from_defaults() {
        assert!(TapResolver::with_defaults().is_ok());
    }

    #[test]
    fn resolver_rejects_invalid_endpoint() {
        let config = SimbadConfig { endpoint: "not a url".to_owned(), ..SimbadConfig::default() };
        let err = TapResolver::new(&config).unwrap_err();
        assert!(matches!(err, ResolveError::Network(_)));
    }
}
