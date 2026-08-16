// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! SIMBAD Sesame resolver (broad-coverage name resolve).
//!
//! [`SesameResolver`] queries CDS Sesame (the `-oxp` XML flavour), which
//! aggregates SIMBAD + NED + VizieR name resolution: broader name coverage
//! than the TAP resolver, but coarser output (no reliable `simbad_oid`, and
//! object-type/alias completeness depends on which backend Sesame answers from
//! first). It optionally enriches a coarse Sesame hit through a caller-supplied
//! [`Resolver`] trait object (typically [`crate::TapResolver`]).

use std::sync::Arc;

use async_trait::async_trait;

use crate::{
    map_otype, normalize, wire, AliasKind, ResolveError, ResolvedAlias, ResolvedIdentity, Resolver,
    SimbadConfig, TargetSource,
};

mod parse;

use parse::SesameHit;

/// Default CDS Sesame `-oxp` endpoint: `S`=SIMBAD, `N`=NED, `V`=VizieR,
/// aggregated in that resolver-precedence order.
///
/// The trailing `?` is intentional and load-bearing: Sesame's CGI query is the
/// percent-encoded object name appended directly after it, not a `key=value`
/// pair (see [`SesameResolver`]'s request building).
pub const DEFAULT_SESAME_ENDPOINT: &str = "https://cds.unistra.fr/cgi-bin/nph-sesame/-oxp/SNV?";

/// Build a [`SimbadConfig`] defaulted to the CDS Sesame endpoint (10s timeout,
/// shared `simbad-resolver` user agent) instead of the default TAP endpoint.
///
/// Crate-internal (the `sesame` module is private); used by
/// [`SesameResolver::new`], which is the public entry point.
#[must_use]
pub fn default_sesame_config() -> SimbadConfig {
    SimbadConfig { endpoint: DEFAULT_SESAME_ENDPOINT.to_owned(), ..SimbadConfig::default() }
}

/// Defensive cap on the Sesame response body (~8 MiB) — bounds memory/parse
/// time against a pathological or misbehaving endpoint; a real Sesame `-oxp`
/// reply for one target is a few KiB.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// SIMBAD Sesame resolver: broad name coverage (SIMBAD + NED + VizieR
/// aggregation via CDS Sesame's `-oxp` XML flavour) at the cost of coarser
/// output (no `simbad_oid`; object-type/alias completeness depends on which
/// backend Sesame answers from first).
///
/// Optionally enriches a coarse Sesame hit through a caller-supplied
/// [`Resolver`] trait object ([`Self::with_enricher`]) — typically
/// [`crate::TapResolver`].
///
/// Queries the real CDS Sesame endpoint, so this example is `no_run` — it
/// needs network access to complete. `Vega` is resolvable by Sesame even
/// though it is not a SIMBAD `NAME …` alias, which is the broader-coverage
/// tradeoff over [`crate::TapResolver`].
///
/// ```no_run
/// use std::sync::Arc;
///
/// use simbad_resolver::{Resolver, SesameResolver, TapResolver};
///
/// # async fn demo() -> Result<(), simbad_resolver::ResolveError> {
/// let enricher: Arc<dyn Resolver> = Arc::new(TapResolver::with_defaults()?);
/// let resolver = SesameResolver::new().with_enricher(enricher);
/// let identity = resolver.resolve("Vega").await?;
/// println!("{} @ ({}, {})", identity.primary_designation, identity.ra_deg, identity.dec_deg);
/// # Ok(()) }
/// ```
pub struct SesameResolver {
    config: SimbadConfig,
    client: reqwest::Client,
    enricher: Option<Arc<dyn Resolver>>,
}

impl SesameResolver {
    /// Build a resolver against the default CDS Sesame endpoint.
    ///
    /// Only builds the HTTP client — no request is sent, so this is runnable.
    ///
    /// ```
    /// use simbad_resolver::SesameResolver;
    ///
    /// let resolver = SesameResolver::new();
    /// # let _ = resolver;
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(default_sesame_config())
    }

    /// Build a resolver against a caller-supplied config (custom endpoint,
    /// timeout, or user agent).
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use simbad_resolver::{SesameResolver, SimbadConfig};
    ///
    /// let config = SimbadConfig {
    ///     endpoint: "https://example.test/sesame?".to_owned(),
    ///     timeout: Duration::from_secs(5),
    ///     ..SimbadConfig::default()
    /// };
    /// let resolver = SesameResolver::with_config(config);
    /// # let _ = resolver;
    /// ```
    #[must_use]
    pub fn with_config(config: SimbadConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .user_agent(config.user_agent.clone())
            .build()
            .unwrap_or_else(|error| {
                tracing::warn!(
                    %error,
                    "invalid Sesame client config; falling back to reqwest defaults"
                );
                reqwest::Client::new()
            });
        Self { config, client, enricher: None }
    }

    /// Attach an optional enricher.
    ///
    /// After a successful Sesame hit, [`Resolver::resolve`] resolves the coarse
    /// hit's primary designation through `enricher`; a successful enrichment
    /// replaces the coarse identity (object type + full alias set), a failed
    /// one falls back to the coarse Sesame identity.
    ///
    /// ```
    /// use std::sync::Arc;
    ///
    /// use simbad_resolver::{Resolver, SesameResolver, TapResolver};
    ///
    /// # fn run() -> Result<(), simbad_resolver::ResolveError> {
    /// let enricher: Arc<dyn Resolver> = Arc::new(TapResolver::with_defaults()?);
    /// let resolver = SesameResolver::new().with_enricher(enricher);
    /// # let _ = resolver;
    /// # Ok(()) }
    /// ```
    #[must_use]
    pub fn with_enricher(mut self, enricher: Arc<dyn Resolver>) -> Self {
        self.enricher = Some(enricher);
        self
    }

    fn request_url(&self, name: &str) -> Result<url::Url, ResolveError> {
        let encoded = parse::percent_encode_query(name);
        let combined = format!("{}{encoded}", self.config.endpoint);
        url::Url::parse(&combined)
            .map_err(|error| ResolveError::Network(format!("invalid Sesame endpoint: {error}")))
    }

    fn classify_transport_error(&self, error: &reqwest::Error) -> ResolveError {
        if error.is_timeout() {
            ResolveError::Timeout(self.config.timeout.as_secs())
        } else {
            ResolveError::Network(error.to_string())
        }
    }
}

impl Default for SesameResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Resolver for SesameResolver {
    async fn resolve(&self, query: &str) -> Result<ResolvedIdentity, ResolveError> {
        let url = self.request_url(query)?;
        let response =
            self.client.get(url).send().await.map_err(|e| self.classify_transport_error(&e))?;
        if !response.status().is_success() {
            return Err(ResolveError::Network(format!(
                "Sesame returned HTTP {}",
                response.status()
            )));
        }
        let body = response.bytes().await.map_err(|e| self.classify_transport_error(&e))?;
        let raw: &[u8] = &body;
        let bounded = if raw.len() > MAX_BODY_BYTES { &raw[..MAX_BODY_BYTES] } else { raw };
        let text = String::from_utf8_lossy(bounded);

        let hit = parse::parse_sesame_xml(&text)
            .ok_or_else(|| ResolveError::NotFound(query.to_owned()))?;
        let coarse = build_identity(&hit, query);

        Ok(resolve_with_enrichment(self.enricher.as_ref(), coarse).await)
    }
}

/// Build the coarse [`ResolvedIdentity`] from a parsed Sesame hit.
///
/// `common_name` is always `None`: Sesame has no analog of SIMBAD's curated
/// `NAME …` identifier, so a caller wanting a common name needs enrichment (or
/// a dedicated lookup) rather than a guess from the alias list.
fn build_identity(hit: &SesameHit, query: &str) -> ResolvedIdentity {
    let primary = hit
        .oname
        .as_deref()
        .map(wire::collapse_spaces)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| wire::collapse_spaces(query));

    let otype_raw = hit.otype.clone().unwrap_or_default();
    let object_type = map_otype(&otype_raw);

    let mut aliases = Vec::new();
    let mut seen = std::collections::HashSet::new();
    push_alias(&primary, &mut aliases, &mut seen);
    for alias in &hit.aliases {
        push_alias(alias, &mut aliases, &mut seen);
    }

    ResolvedIdentity {
        simbad_oid: None,
        primary_designation: primary,
        common_name: None,
        object_type,
        otype_raw,
        ra_deg: hit.ra_deg,
        dec_deg: hit.dec_deg,
        // Sesame is a name→position service and carries no photometry or
        // angular-size data — only TAP (`basic.galdim_*`) supplies that.
        v_mag: None,
        galdim_majaxis_arcmin: None,
        galdim_minaxis_arcmin: None,
        galdim_angle_deg: None,
        aliases,
        source: TargetSource::Resolved,
    }
}

/// Push `name` as a designation alias, deduping by normalized form (Sesame's
/// alias list commonly repeats the primary designation).
fn push_alias(
    name: &str,
    aliases: &mut Vec<ResolvedAlias>,
    seen: &mut std::collections::HashSet<String>,
) {
    let cleaned = wire::collapse_spaces(name);
    if cleaned.is_empty() {
        return;
    }
    if seen.insert(normalize::normalize(&cleaned)) {
        aliases.push(ResolvedAlias::new(cleaned, AliasKind::Designation));
    }
}

/// Resolve a coarse Sesame identity through the optional enricher, falling back
/// to the coarse identity when unset or on enrichment failure.
///
/// Kept network-free (the caller passes an already-fetched `coarse` identity)
/// so the enrichment decision logic is unit-testable with a `FakeResolver`,
/// with no HTTP mocking required.
async fn resolve_with_enrichment(
    enricher: Option<&Arc<dyn Resolver>>,
    coarse: ResolvedIdentity,
) -> ResolvedIdentity {
    let Some(enricher) = enricher else { return coarse };
    match enricher.resolve(&coarse.primary_designation).await {
        Ok(mut identity) => {
            identity.source = TargetSource::Resolved;
            identity
        }
        Err(_) => coarse,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FakeResolver, ObjectType};

    fn coarse_m31() -> ResolvedIdentity {
        ResolvedIdentity {
            simbad_oid: None,
            primary_designation: "M 31".to_owned(),
            common_name: None,
            object_type: ObjectType::Other,
            otype_raw: String::new(),
            ra_deg: 10.684_708,
            dec_deg: 41.268_75,
            v_mag: None,
            galdim_majaxis_arcmin: None,
            galdim_minaxis_arcmin: None,
            galdim_angle_deg: None,
            aliases: vec![ResolvedAlias::new("M 31", AliasKind::Designation)],
            source: TargetSource::Resolved,
        }
    }

    fn enriched_m31() -> ResolvedIdentity {
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

    #[test]
    fn default_sesame_config_uses_sesame_endpoint() {
        let config = default_sesame_config();
        assert_eq!(config.endpoint, DEFAULT_SESAME_ENDPOINT);
        assert!(config.endpoint.ends_with('?'));
    }

    #[test]
    fn build_identity_dedupes_primary_from_alias_list() {
        let hit = SesameHit {
            ra_deg: 10.684_708,
            dec_deg: 41.268_75,
            oname: Some("M  31".to_owned()),
            otype: Some("G".to_owned()),
            aliases: vec!["M  31".to_owned(), "NGC  224".to_owned()],
        };
        let identity = build_identity(&hit, "m31");
        assert_eq!(identity.primary_designation, "M 31");
        assert_eq!(identity.object_type, ObjectType::Galaxy);
        assert_eq!(identity.common_name, None);
        assert_eq!(identity.simbad_oid, None);
        assert_eq!(identity.source, TargetSource::Resolved);
        assert_eq!(
            identity.aliases.iter().map(|a| a.alias.as_str()).collect::<Vec<_>>(),
            vec!["M 31", "NGC 224"]
        );
    }

    #[test]
    fn build_identity_falls_back_to_query_when_oname_missing() {
        let hit =
            SesameHit { ra_deg: 1.0, dec_deg: 1.0, oname: None, otype: None, aliases: vec![] };
        let identity = build_identity(&hit, "NGC 1");
        assert_eq!(identity.primary_designation, "NGC 1");
        assert_eq!(identity.object_type, ObjectType::Other);
        assert_eq!(identity.otype_raw, "");
    }

    #[tokio::test]
    async fn enrichment_overrides_coarse_result() {
        let enriched = enriched_m31();
        let fake = Arc::new(FakeResolver::new().with_response("M 31", enriched.clone()))
            as Arc<dyn Resolver>;
        let got = resolve_with_enrichment(Some(&fake), coarse_m31()).await;
        assert_eq!(got, enriched);
        assert_eq!(got.object_type, ObjectType::Galaxy);
    }

    #[tokio::test]
    async fn enrichment_failure_falls_back_to_coarse() {
        let fake = Arc::new(
            FakeResolver::new().with_error("M 31", ResolveError::NotFound("M 31".to_owned())),
        ) as Arc<dyn Resolver>;
        let got = resolve_with_enrichment(Some(&fake), coarse_m31()).await;
        assert_eq!(got, coarse_m31());
    }

    #[tokio::test]
    async fn no_enricher_returns_coarse_unchanged() {
        let got = resolve_with_enrichment(None, coarse_m31()).await;
        assert_eq!(got, coarse_m31());
    }

    #[test]
    fn request_url_percent_encodes_name_after_literal_question_mark() {
        let resolver = SesameResolver::new();
        let url = resolver.request_url("M 31").unwrap();
        assert_eq!(url.as_str(), "https://cds.unistra.fr/cgi-bin/nph-sesame/-oxp/SNV?M%2031");
    }

    #[test]
    fn with_config_uses_custom_endpoint() {
        let config = SimbadConfig {
            endpoint: "https://example.test/sesame?".to_owned(),
            ..default_sesame_config()
        };
        let resolver = SesameResolver::with_config(config);
        let url = resolver.request_url("NGC 224").unwrap();
        assert_eq!(url.as_str(), "https://example.test/sesame?NGC%20224");
    }
}
