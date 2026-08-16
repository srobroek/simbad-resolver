// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Cache-first resolve with no network access.
//!
//! Mirrors the "Test without the network" section of `docs/guide.md`: seeds
//! the cache directly with the `M 31` fixture, then resolves it through
//! [`OfflineResolver`] — a `Resolver` that always reports
//! [`simbad_resolver::ResolveError::Disabled`] but is never actually called
//! here, because `SimbadResolver::resolve` is cache-first.
//!
//! Run with `cargo run --example resolve_offline`.

use simbad_resolver::{
    AliasKind, CacheBackend, ObjectType, OfflineResolver, Resolution, ResolvedAlias,
    ResolvedIdentity, ResolverConfig, SimbadResolver, TargetSource,
};

#[tokio::main]
async fn main() -> Result<(), simbad_resolver::Error> {
    let config = ResolverConfig::new("examples.resolve_offline");
    let namespace = config.namespace;
    let facade = SimbadResolver::new(OfflineResolver, CacheBackend::InMemory, config)?;

    let m31 = ResolvedIdentity {
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
        aliases: vec![ResolvedAlias::new("M 31", AliasKind::Designation)],
        source: TargetSource::Seed,
    };
    facade.cache().upsert(&m31, &namespace).await?;

    match facade.resolve("M31").await? {
        Resolution::Resolved(target) => {
            println!(
                "{} ({}) at ({}, {})",
                target.primary_designation,
                target.common_name.as_deref().unwrap_or("no common name"),
                target.ra_deg,
                target.dec_deg
            );
        }
        Resolution::Unresolved { reason, .. } => {
            unreachable!("M 31 was seeded into the cache above, got {reason:?}")
        }
    }

    Ok(())
}
