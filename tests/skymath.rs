// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Typed-coordinate accessor tests (`position()` -> `skymath::Equatorial`).
//!
//! Constructs the resolved-object types directly; no network.

use simbad_resolver::{
    AliasKind, CachedTarget, ObjectType, PositionMatch, ResolvedAlias, ResolvedIdentity,
    TargetSource,
};

fn m31_identity(dec_deg: f64) -> ResolvedIdentity {
    ResolvedIdentity {
        simbad_oid: Some(1_575_544),
        primary_designation: "M 31".to_owned(),
        common_name: Some("Andromeda Galaxy".to_owned()),
        object_type: ObjectType::Galaxy,
        otype_raw: "G".to_owned(),
        ra_deg: 10.684_708,
        dec_deg,
        v_mag: Some(3.44),
        galdim_majaxis_arcmin: Some(199.53),
        galdim_minaxis_arcmin: Some(70.79),
        galdim_angle_deg: Some(35),
        aliases: vec![ResolvedAlias::new("M 31", AliasKind::Designation)],
        source: TargetSource::Resolved,
    }
}

#[test]
fn resolved_identity_position_round_trips() {
    let id = m31_identity(41.268_75);
    let eq = id.position().expect("valid coordinates");
    assert!((eq.ra().degrees() - id.ra_deg).abs() < 1e-9, "ra round-trips");
    assert!((eq.dec().degrees() - id.dec_deg).abs() < 1e-9, "dec round-trips");
}

#[test]
fn out_of_range_declination_is_err() {
    // Dec = 91° is outside [-90, +90]; the typed constructor must reject it
    // rather than fabricate an invalid coordinate.
    assert!(m31_identity(91.0).position().is_err());
}

#[test]
fn cached_target_position_round_trips() {
    let target = CachedTarget {
        id: uuid::Uuid::nil(),
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
        resolved_at: "2026-07-12T00:00:00Z".to_owned(),
        aliases: vec![],
    };
    let eq = target.position().expect("valid coordinates");
    assert!((eq.ra().degrees() - target.ra_deg).abs() < 1e-9);
    assert!((eq.dec().degrees() - target.dec_deg).abs() < 1e-9);
}

#[test]
fn position_match_delegates_to_identity() {
    let hit = PositionMatch { identity: m31_identity(41.268_75), separation_deg: 0.5 };
    let eq = hit.position().expect("valid coordinates");
    assert!((eq.dec().degrees() - 41.268_75).abs() < 1e-9);

    // An out-of-range identity propagates the error through the delegate.
    let bad = PositionMatch { identity: m31_identity(91.0), separation_deg: 0.5 };
    assert!(bad.position().is_err());
}
