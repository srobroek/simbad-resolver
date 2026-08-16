// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Offline HTTP-level tests for `TapResolver`, exercising the full
//! `resolve` / `resolve_position` paths and every transport branch against a
//! `wiremock` mock of the TAP sync endpoint — no network, so these run in CI
//! (the `tests/live.rs` suite covers the same paths against real SIMBAD but is
//! `#[ignore]`-gated).

use std::time::Duration;

use simbad_resolver::{
    ObjectType, PositionResolver, ResolveError, Resolver, SimbadConfig, TapResolver,
};
use wiremock::matchers::{header, method, query_param_contains};
use wiremock::{Mock, MockServer, ResponseTemplate};

const UA: &str = "simbad-resolver-test/9.9";

/// First round-trip (`basic ⋈ ident`) with one matching object.
const M31_BASIC_TSV: &str =
    "oid\tmain_id\tra\tdec\totype_txt\tV\tgaldim_majaxis\tgaldim_minaxis\tgaldim_angle\n\
    1575544\t\"M  31\"\t10.6847083\t41.26875\t\"G\"\t3.44\t199.53\t70.79\t35\n";
/// M 57: a near-circular planetary nebula, equal major/minor axes.
const M57_BASIC_TSV: &str =
    "oid\tmain_id\tra\tdec\totype_txt\tV\tgaldim_majaxis\tgaldim_minaxis\tgaldim_angle\n\
    1560141\t\"M  57\"\t283.3963\t33.0292\t\"PN\"\t8.79\t1.153\t1.153\t90\n";
/// Vega: a point source with no galdim measurement at all → NULL columns.
const VEGA_BASIC_TSV: &str =
    "oid\tmain_id\tra\tdec\totype_txt\tV\tgaldim_majaxis\tgaldim_minaxis\tgaldim_angle\n\
    3672809\t\"* alf Lyr\"\t279.2347\t38.7837\t\"*\"\t0.03\t\t\t\n";
/// First round-trip with two distinct physical objects → ambiguous.
const AMBIGUOUS_BASIC_TSV: &str =
    "oid\tmain_id\tra\tdec\totype_txt\tV\tgaldim_majaxis\tgaldim_minaxis\tgaldim_angle\n\
    1575544\t\"M  31\"\t10.6847083\t41.26875\t\"G\"\t3.44\t199.53\t70.79\t35\n\
    999999\t\"Some Other\"\t11.0\t42.0\t\"G\"\t\t\t\t\n";
/// First round-trip with header only → no object.
const EMPTY_BASIC_TSV: &str =
    "oid\tmain_id\tra\tdec\totype_txt\tV\tgaldim_majaxis\tgaldim_minaxis\tgaldim_angle\n";
/// Second round-trip (`ident` for the winning oid) → alias set.
const M31_ALIAS_TSV: &str = "id\n\
    \"M   31\"\n\
    \"NGC  224\"\n\
    \"NAME Andromeda Galaxy\"\n";
/// M 57's alias round-trip.
const M57_ALIAS_TSV: &str = "id\n\
    \"M   57\"\n\
    \"NGC  6720\"\n";
/// Vega's alias round-trip.
const VEGA_ALIAS_TSV: &str = "id\n\
    \"* alf Lyr\"\n\
    \"NAME Vega\"\n";
/// Cone-search response, already ordered nearest-first by the server. NGC 206
/// (an open cluster) has no V mag or galdim measurement → empty trailing
/// columns before `dist`.
const POSITION_TSV: &str =
    "oid\tmain_id\tra\tdec\totype_txt\tV\tgaldim_majaxis\tgaldim_minaxis\tgaldim_angle\tdist\n\
    1575544\t\"M  31\"\t10.6847083\t41.26875\t\"G\"\t3.44\t199.53\t70.79\t35\t0.001\n\
    222\t\"NGC 206\"\t10.9\t40.7\t\"OpC\"\t\t\t\t\t0.6\n";
/// A TAP error returned as a VOTable body under HTTP 200.
const VOTABLE_ERROR_BODY: &str =
    "<VOTABLE version=\"1.4\"><RESOURCE><INFO name=\"QUERY_STATUS\" value=\"ERROR\">\
     syntax error</INFO></RESOURCE></VOTABLE>";

fn resolver_at(server: &MockServer, timeout: Duration) -> TapResolver {
    let config = SimbadConfig {
        endpoint: format!("{}/sim-tap/sync", server.uri()),
        timeout,
        user_agent: UA.to_owned(),
    };
    TapResolver::new(&config).expect("resolver builds from mock endpoint")
}

fn resolver(server: &MockServer) -> TapResolver {
    resolver_at(server, Duration::from_secs(5))
}

/// Mount the first-round-trip (`SELECT DISTINCT ...`) response.
async fn mount_find(server: &MockServer, body: &'static str) {
    Mock::given(method("GET"))
        .and(query_param_contains("query", "SELECT DISTINCT"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(server)
        .await;
}

/// Mount the alias round-trip (`SELECT i.id ...`) response.
async fn mount_aliases(server: &MockServer, body: &'static str) {
    Mock::given(method("GET"))
        .and(query_param_contains("query", "SELECT i.id"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(server)
        .await;
}

#[tokio::test]
async fn resolve_single_object_returns_full_identity() {
    let server = MockServer::start().await;
    mount_find(&server, M31_BASIC_TSV).await;
    mount_aliases(&server, M31_ALIAS_TSV).await;

    let identity = resolver(&server).resolve("M 31").await.expect("M 31 resolves");

    assert_eq!(identity.simbad_oid, Some(1_575_544));
    assert_eq!(identity.primary_designation, "M 31");
    assert_eq!(identity.object_type, ObjectType::Galaxy);
    assert!((identity.ra_deg - 10.684_708_3).abs() < 1e-6);
    assert!((identity.dec_deg - 41.268_75).abs() < 1e-6);
    assert_eq!(identity.v_mag, Some(3.44), "V mag enriched from the allfluxes join");
    assert_eq!(identity.galdim_majaxis_arcmin, Some(199.53), "M 31 major axis, arcmin");
    assert_eq!(identity.galdim_minaxis_arcmin, Some(70.79), "M 31 minor axis, arcmin");
    assert_eq!(identity.galdim_angle_deg, Some(35), "M 31 position angle, degrees");
    assert_eq!(identity.common_name.as_deref(), Some("Andromeda Galaxy"));
    assert!(identity.aliases.iter().any(|a| a.alias == "NGC 224"));
    assert!(identity.aliases.iter().any(|a| a.alias == "M 31"));
}

#[tokio::test]
async fn resolve_equal_axes_object_reports_matching_majaxis_and_minaxis() {
    let server = MockServer::start().await;
    mount_find(&server, M57_BASIC_TSV).await;
    mount_aliases(&server, M57_ALIAS_TSV).await;

    let identity = resolver(&server).resolve("M 57").await.expect("M 57 resolves");

    assert_eq!(identity.galdim_majaxis_arcmin, Some(1.153));
    assert_eq!(identity.galdim_minaxis_arcmin, Some(1.153));
    assert_eq!(identity.galdim_angle_deg, Some(90));
}

#[tokio::test]
async fn resolve_point_source_has_no_galdim_measurement() {
    // Vega is a point source: SIMBAD has no angular-size measurement for it at
    // all. This must read back as None on every galdim field, never 0.0/0 —
    // "no measurement" and "measured as zero" are different facts.
    let server = MockServer::start().await;
    mount_find(&server, VEGA_BASIC_TSV).await;
    mount_aliases(&server, VEGA_ALIAS_TSV).await;

    let identity = resolver(&server).resolve("Vega").await.expect("Vega resolves");

    assert_eq!(identity.galdim_majaxis_arcmin, None);
    assert_eq!(identity.galdim_minaxis_arcmin, None);
    assert_eq!(identity.galdim_angle_deg, None);
}

#[tokio::test]
async fn resolve_zero_rows_is_not_found() {
    let server = MockServer::start().await;
    mount_find(&server, EMPTY_BASIC_TSV).await;

    let err = resolver(&server).resolve("No Such Object").await.unwrap_err();
    assert!(matches!(err, ResolveError::NotFound(q) if q == "No Such Object"));
}

#[tokio::test]
async fn resolve_multiple_rows_is_ambiguous() {
    let server = MockServer::start().await;
    mount_find(&server, AMBIGUOUS_BASIC_TSV).await;

    let err = resolver(&server).resolve("Ambiguous").await.unwrap_err();
    assert!(matches!(err, ResolveError::Ambiguous { count: 2, .. }));
}

#[tokio::test]
async fn resolve_empty_query_short_circuits_without_a_request() {
    let server = MockServer::start().await;
    // No mock mounted: a NotFound must come from the early return, not a call.
    let err = resolver(&server).resolve("   ").await.unwrap_err();
    assert!(matches!(err, ResolveError::NotFound(q) if q.is_empty()));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn resolve_votable_error_under_http_200_is_parse_error() {
    let server = MockServer::start().await;
    mount_find(&server, VOTABLE_ERROR_BODY).await;

    let err = resolver(&server).resolve("M 31").await.unwrap_err();
    assert!(matches!(err, ResolveError::Parse(_)));
}

#[tokio::test]
async fn resolve_http_5xx_is_network_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(500)).mount(&server).await;

    let err = resolver(&server).resolve("M 31").await.unwrap_err();
    assert!(matches!(err, ResolveError::Network(_)));
}

#[tokio::test]
async fn resolve_slow_endpoint_maps_to_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(EMPTY_BASIC_TSV)
                .set_delay(Duration::from_secs(3)),
        )
        .mount(&server)
        .await;

    let err = resolver_at(&server, Duration::from_millis(150)).resolve("M 31").await.unwrap_err();
    assert!(matches!(err, ResolveError::Timeout(_)));
}

#[tokio::test]
async fn resolve_transmits_configured_user_agent() {
    // The mount matches ONLY when the configured User-Agent is present; a
    // missing UA would 404 → Network error rather than the NotFound we assert,
    // so a passing NotFound proves the header was sent (FR-014).
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(header("user-agent", UA))
        .and(query_param_contains("query", "SELECT DISTINCT"))
        .respond_with(ResponseTemplate::new(200).set_body_string(EMPTY_BASIC_TSV))
        .expect(1)
        .mount(&server)
        .await;

    let err = resolver(&server).resolve("Anything").await.unwrap_err();
    assert!(matches!(err, ResolveError::NotFound(_)));
}

#[tokio::test]
async fn resolve_position_returns_matches_ordered_nearest_first() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param_contains("query", "CONTAINS"))
        .respond_with(ResponseTemplate::new(200).set_body_string(POSITION_TSV))
        .mount(&server)
        .await;

    let matches = resolver(&server)
        .resolve_position(10.68, 41.27, 1.0, 5)
        .await
        .expect("cone search succeeds");

    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0].identity.simbad_oid, Some(1_575_544));
    assert_eq!(matches[0].identity.v_mag, Some(3.44), "cone hit carries V mag");
    assert_eq!(matches[0].identity.galdim_majaxis_arcmin, Some(199.53), "cone hit carries galdim");
    assert_eq!(matches[0].identity.galdim_minaxis_arcmin, Some(70.79));
    assert_eq!(matches[0].identity.galdim_angle_deg, Some(35));
    assert!((matches[0].separation_deg - 0.001).abs() < 1e-9);
    assert!(matches[0].separation_deg <= matches[1].separation_deg);
    // A cone hit carries its primary designation as its sole alias.
    assert!(matches[1].identity.aliases.iter().any(|a| a.alias == "NGC 206"));
    assert_eq!(matches[1].identity.v_mag, None, "NGC 206 has no V photometry");
    assert_eq!(
        matches[1].identity.galdim_majaxis_arcmin, None,
        "NGC 206 has no galdim measurement"
    );
}
