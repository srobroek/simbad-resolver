// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Offline HTTP-level tests for `SesameResolver` against a `wiremock` mock of
//! the CDS Sesame `-oxp` endpoint — no network, so these run in CI
//! (`tests/live.rs` covers the same path against real Sesame but is
//! `#[ignore]`-gated).
//!
//! The success fixtures reproduce the *real* live `-oxp/SNV` response shape
//! (a `<Target option="SNV">` wrapper and, notably, **no `<alias>` elements** —
//! standalone Sesame returns coordinates + primary name + object type only;
//! aliases require an enricher), rather than the hand-built shape in the
//! crate's unit fixture.

use std::sync::Arc;
use std::time::Duration;

use simbad_resolver::{
    AliasKind, FakeResolver, ObjectType, ResolveError, ResolvedAlias, ResolvedIdentity, Resolver,
    SesameResolver, SimbadConfig, TargetSource,
};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const UA: &str = "simbad-resolver-test/9.9";

/// A live-shaped `-oxp/SNV` reply for M 31: one Simbad `<Resolver>` block,
/// coordinates + `oname` + `otype`, and no `<alias>` elements.
const M31_SNV_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Sesame xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
<Target option="SNV">
  <name>M31</name>
  <Resolver name="Sca=Simbad (CfA, via client/server)">
    <oid>1575544</oid>
    <otype>AGN</otype>
    <jpos>00:42:44.32 +41:16:07.5</jpos>
    <jradeg>10.68470833</jradeg>
    <jdedeg>41.26875</jdedeg>
    <oname>M  31</oname>
  </Resolver>
</Target>
</Sesame>
"#;

/// A reply whose first `<Resolver>` block has out-of-range coordinates and
/// whose second block is valid — the parser must skip the first and pick the
/// second.
const FIRST_BLOCK_BAD_XML: &str = r#"<?xml version="1.0"?>
<Sesame>
<Target option="SNV">
  <name>M31</name>
  <Resolver name="N=NED">
    <jradeg>999.0</jradeg>
    <jdedeg>0.0</jdedeg>
    <oname>bad row</oname>
  </Resolver>
  <Resolver name="S=Simbad">
    <jradeg>10.68470833</jradeg>
    <jdedeg>41.26875</jdedeg>
    <oname>M  31</oname>
  </Resolver>
</Target>
</Sesame>
"#;

/// A reply with no usable coordinates (Sesame found nothing).
const NOTHING_FOUND_XML: &str = r#"<?xml version="1.0"?>
<Sesame>
<Target option="SNV">
  <name>Bogus Name Xyz</name>
  <Resolver name="S=Simbad"><INFO>*** nothing found ***</INFO></Resolver>
</Target>
</Sesame>
"#;

fn config_at(server: &MockServer, timeout: Duration) -> SimbadConfig {
    SimbadConfig {
        endpoint: format!("{}/sesame?", server.uri()),
        timeout,
        user_agent: UA.to_owned(),
    }
}

async fn mount_body(server: &MockServer, body: &'static str) {
    Mock::given(method("GET"))
        .and(path("/sesame"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(server)
        .await;
}

#[tokio::test]
async fn resolve_live_shaped_response_yields_coords_name_and_type() {
    let server = MockServer::start().await;
    mount_body(&server, M31_SNV_XML).await;

    let resolver = SesameResolver::with_config(config_at(&server, Duration::from_secs(5)));
    let identity = resolver.resolve("M 31").await.expect("M 31 resolves");

    assert_eq!(identity.primary_designation, "M 31");
    assert!((identity.ra_deg - 10.684_708_33).abs() < 1e-6);
    assert!((identity.dec_deg - 41.268_75).abs() < 1e-6);
    // SIMBAD's `AGN` code maps into the closed Galaxy bucket; the raw code is
    // preserved on `otype_raw`.
    assert_eq!(identity.object_type, ObjectType::Galaxy);
    assert_eq!(identity.otype_raw, "AGN");
    assert_eq!(identity.simbad_oid, None, "Sesame identities carry no oid");
    assert_eq!(identity.common_name, None, "standalone Sesame has no curated name");
    // No <alias> elements in a real SNV reply → only the primary designation.
    assert_eq!(identity.aliases.len(), 1);
    assert_eq!(identity.aliases[0].alias, "M 31");
}

#[tokio::test]
async fn resolve_skips_first_block_with_bad_coordinates() {
    let server = MockServer::start().await;
    mount_body(&server, FIRST_BLOCK_BAD_XML).await;

    let resolver = SesameResolver::with_config(config_at(&server, Duration::from_secs(5)));
    let identity = resolver.resolve("M 31").await.expect("second block resolves");

    assert_eq!(identity.primary_designation, "M 31");
    assert!((identity.ra_deg - 10.684_708_33).abs() < 1e-6);
}

#[tokio::test]
async fn resolve_unparseable_response_is_not_found() {
    let server = MockServer::start().await;
    mount_body(&server, NOTHING_FOUND_XML).await;

    let resolver = SesameResolver::with_config(config_at(&server, Duration::from_secs(5)));
    let err = resolver.resolve("Bogus Name Xyz").await.unwrap_err();
    assert!(matches!(err, ResolveError::NotFound(q) if q == "Bogus Name Xyz"));
}

#[tokio::test]
async fn resolve_http_error_maps_to_network() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sesame"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let resolver = SesameResolver::with_config(config_at(&server, Duration::from_secs(5)));
    let err = resolver.resolve("M 31").await.unwrap_err();
    assert!(matches!(err, ResolveError::Network(_)));
}

#[tokio::test]
async fn resolve_slow_endpoint_maps_to_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sesame"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(M31_SNV_XML)
                .set_delay(Duration::from_secs(3)),
        )
        .mount(&server)
        .await;

    let resolver = SesameResolver::with_config(config_at(&server, Duration::from_millis(150)));
    let err = resolver.resolve("M 31").await.unwrap_err();
    assert!(matches!(err, ResolveError::Timeout(_)));
}

#[tokio::test]
async fn resolve_with_enricher_replaces_coarse_identity() {
    let server = MockServer::start().await;
    mount_body(&server, M31_SNV_XML).await;

    let tap_identity = ResolvedIdentity {
        simbad_oid: Some(1_575_544),
        primary_designation: "M 31".to_owned(),
        common_name: Some("Andromeda Galaxy".to_owned()),
        object_type: ObjectType::Galaxy,
        otype_raw: "G".to_owned(),
        ra_deg: 10.684_708_33,
        dec_deg: 41.268_75,
        v_mag: Some(3.44),
        galdim_majaxis_arcmin: Some(199.53),
        galdim_minaxis_arcmin: Some(70.79),
        galdim_angle_deg: Some(35),
        aliases: vec![
            ResolvedAlias::new("M 31", AliasKind::Designation),
            ResolvedAlias::new("NGC 224", AliasKind::Designation),
        ],
        source: TargetSource::Resolved,
    };
    let enricher = Arc::new(FakeResolver::new().with_response("M 31", tap_identity.clone()))
        as Arc<dyn Resolver>;
    let resolver = SesameResolver::with_config(config_at(&server, Duration::from_secs(5)))
        .with_enricher(enricher);

    let identity = resolver.resolve("M 31").await.expect("resolves and enriches");
    assert_eq!(identity.simbad_oid, Some(1_575_544));
    assert_eq!(identity.object_type, ObjectType::Galaxy);
    assert!(identity.aliases.iter().any(|a| a.alias == "NGC 224"));
}

#[tokio::test]
async fn resolve_transmits_configured_user_agent() {
    // Mount matches only when the configured UA header is present (FR-014).
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sesame"))
        .and(header("user-agent", UA))
        .respond_with(ResponseTemplate::new(200).set_body_string(NOTHING_FOUND_XML))
        .expect(1)
        .mount(&server)
        .await;

    let resolver = SesameResolver::with_config(config_at(&server, Duration::from_secs(5)));
    let err = resolver.resolve("Whatever").await.unwrap_err();
    assert!(matches!(err, ResolveError::NotFound(_)));
}
