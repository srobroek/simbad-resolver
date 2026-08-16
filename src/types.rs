// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Resolved-identity types: [`ObjectType`], [`TargetSource`], [`AliasKind`],
//! [`ResolvedAlias`], [`ResolvedIdentity`], [`PositionMatch`].

use serde::{Deserialize, Serialize};

// ── ObjectType ───────────────────────────────────────────────────────────────

/// Closed object-type enum mapped from SIMBAD `otype`.
///
/// Serialized as `snake_case`. Any SIMBAD `otype` outside the closed set maps
/// to [`ObjectType::Other`] (see [`map_otype`]); the raw string is preserved
/// separately on [`ResolvedIdentity::otype_raw`] for consumers needing finer
/// types.
///
/// ```
/// use simbad_resolver::ObjectType;
///
/// assert_eq!(ObjectType::Galaxy.as_wire(), "galaxy");
/// assert_eq!(ObjectType::from_wire("planetary_nebula"), ObjectType::PlanetaryNebula);
/// assert_eq!(ObjectType::from_wire("bogus"), ObjectType::Other); // unknown → Other
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectType {
    /// A galaxy (incl. interacting/active galaxy subtypes).
    Galaxy,
    /// A planetary nebula.
    PlanetaryNebula,
    /// An emission nebula (H II region, emission object).
    EmissionNebula,
    /// A reflection nebula.
    ReflectionNebula,
    /// A dark/molecular cloud.
    DarkNebula,
    /// An open/galactic cluster.
    OpenCluster,
    /// A globular cluster.
    GlobularCluster,
    /// A supernova remnant.
    SupernovaRemnant,
    /// A cluster of galaxies.
    GalaxyCluster,
    /// A double/multiple star.
    DoubleStar,
    /// A visual asterism.
    Asterism,
    /// Any object type outside the closed set above.
    Other,
}

impl ObjectType {
    /// The `snake_case` wire/DB string for this object type.
    ///
    /// ```
    /// use simbad_resolver::ObjectType;
    ///
    /// assert_eq!(ObjectType::GalaxyCluster.as_wire(), "galaxy_cluster");
    /// ```
    #[must_use]
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Galaxy => "galaxy",
            Self::PlanetaryNebula => "planetary_nebula",
            Self::EmissionNebula => "emission_nebula",
            Self::ReflectionNebula => "reflection_nebula",
            Self::DarkNebula => "dark_nebula",
            Self::OpenCluster => "open_cluster",
            Self::GlobularCluster => "globular_cluster",
            Self::SupernovaRemnant => "supernova_remnant",
            Self::GalaxyCluster => "galaxy_cluster",
            Self::DoubleStar => "double_star",
            Self::Asterism => "asterism",
            Self::Other => "other",
        }
    }

    /// Parse a wire/DB string into an [`ObjectType`]. Unknown strings map to
    /// [`ObjectType::Other`] (closed enum, forward-compatible read).
    ///
    /// ```
    /// use simbad_resolver::ObjectType;
    ///
    /// assert_eq!(ObjectType::from_wire("globular_cluster"), ObjectType::GlobularCluster);
    /// assert_eq!(ObjectType::from_wire("not-a-real-type"), ObjectType::Other);
    /// ```
    #[must_use]
    pub fn from_wire(s: &str) -> Self {
        match s {
            "galaxy" => Self::Galaxy,
            "planetary_nebula" => Self::PlanetaryNebula,
            "emission_nebula" => Self::EmissionNebula,
            "reflection_nebula" => Self::ReflectionNebula,
            "dark_nebula" => Self::DarkNebula,
            "open_cluster" => Self::OpenCluster,
            "globular_cluster" => Self::GlobularCluster,
            "supernova_remnant" => Self::SupernovaRemnant,
            "galaxy_cluster" => Self::GalaxyCluster,
            "double_star" => Self::DoubleStar,
            "asterism" => Self::Asterism,
            _ => Self::Other,
        }
    }
}

/// Map a raw SIMBAD `otype` string to the closed [`ObjectType`] enum.
///
/// SIMBAD `otype` values are short, case-sensitive condensed codes (e.g. `G`
/// for galaxy, `PN` for planetary nebula, `HII` for an emission/H II region).
/// The mapping is total: any unrecognised or unmapped `otype` (including the
/// empty string) returns [`ObjectType::Other`] so an identity is never
/// dropped for lack of a type.
///
/// The recognised codes follow the SIMBAD object-type vocabulary
/// (<https://simbad.cds.unistra.fr/guide/otypes.htx>); the long-form labels
/// SIMBAD also emits are accepted as aliases for robustness.
///
/// ```
/// use simbad_resolver::{map_otype, ObjectType};
///
/// assert_eq!(map_otype("G"), ObjectType::Galaxy);
/// assert_eq!(map_otype("HII"), ObjectType::EmissionNebula);
/// assert_eq!(map_otype("*"), ObjectType::Other); // a plain star falls outside the closed set
/// ```
#[must_use]
pub fn map_otype(otype: &str) -> ObjectType {
    match otype.trim() {
        // Galaxies (incl. interacting/active galaxy subtypes).
        "G" | "GiC" | "GiG" | "GiP" | "IG" | "PaG" | "AGN" | "SBG" | "rG" | "LSB" | "AG?"
        | "EmG" | "BiC" | "H2G" | "Sy1" | "Sy2" | "SyG" | "Galaxy" => ObjectType::Galaxy,
        // Planetary nebulae.
        "PN" | "PN?" | "pA*" | "PlanetaryNebula" => ObjectType::PlanetaryNebula,
        // Emission nebulae (H II regions, emission objects).
        "HII" | "EmO" | "ISM" | "RNe?" | "EmissionNebula" => ObjectType::EmissionNebula,
        // Reflection nebulae.
        "RNe" | "ReflectionNebula" => ObjectType::ReflectionNebula,
        // Dark / molecular clouds.
        "DNe" | "MoC" | "glb" | "cor" | "GNe" | "DarkNebula" => ObjectType::DarkNebula,
        // Open / galactic clusters.
        "OpC" | "Cl*" | "As*" | "OpenCluster" => ObjectType::OpenCluster,
        // Globular clusters.
        "GlC" | "GlobularCluster" => ObjectType::GlobularCluster,
        // Supernova remnants.
        "SNR" | "SNR?" | "SuperNovaRemnant" | "SupernovaRemnant" => ObjectType::SupernovaRemnant,
        // Clusters of galaxies.
        "ClG" | "GrG" | "CGG" | "SCG" | "GalaxyCluster" => ObjectType::GalaxyCluster,
        // Double / multiple stars.
        "**" | "**?" | "EB*" | "SB*" | "DoubleStar" => ObjectType::DoubleStar,
        // Asterisms (note: SIMBAD uses `As*` for stellar associations; the
        // visual-asterism sense is matched by the long-form label only).
        "Asterism" => ObjectType::Asterism,
        _ => ObjectType::Other,
    }
}

// ── TargetSource ─────────────────────────────────────────────────────────────

/// Provenance of a resolved identity.
///
/// The `UserOverride` variant serializes with the hyphenated `user-override`
/// wire/DB value.
///
/// ```
/// use simbad_resolver::TargetSource;
///
/// // A `user-override` row is sticky: nothing lower-precedence may overwrite it.
/// assert!(TargetSource::UserOverride.may_overwrite(TargetSource::Resolved));
/// assert!(!TargetSource::Resolved.may_overwrite(TargetSource::UserOverride));
/// assert_eq!(TargetSource::UserOverride.as_wire(), "user-override");
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetSource {
    /// Loaded from a bundled seed index at first run.
    Seed,
    /// Resolved live from SIMBAD.
    Resolved,
    /// A manual user correction; wins over `seed`/`resolved`.
    #[serde(rename = "user-override")]
    UserOverride,
}

impl TargetSource {
    /// The wire/DB string for this source (`UserOverride` is the hyphenated
    /// `user-override`).
    ///
    /// ```
    /// use simbad_resolver::TargetSource;
    ///
    /// assert_eq!(TargetSource::UserOverride.as_wire(), "user-override");
    /// assert_eq!(TargetSource::Seed.as_wire(), "seed");
    /// ```
    #[must_use]
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Seed => "seed",
            Self::Resolved => "resolved",
            Self::UserOverride => "user-override",
        }
    }

    /// Parse a wire/DB string into a [`TargetSource`]. Returns `None` for an
    /// unrecognised value.
    ///
    /// ```
    /// use simbad_resolver::TargetSource;
    ///
    /// assert_eq!(TargetSource::from_wire("user-override"), Some(TargetSource::UserOverride));
    /// assert_eq!(TargetSource::from_wire("bogus"), None);
    /// ```
    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "seed" => Some(Self::Seed),
            "resolved" => Some(Self::Resolved),
            "user-override" => Some(Self::UserOverride),
            _ => None,
        }
    }

    /// Source precedence rank for conflicting writes: higher wins.
    /// `user-override` (2) > `resolved` (1) > `seed` (0).
    ///
    /// ```
    /// use simbad_resolver::TargetSource;
    ///
    /// assert!(TargetSource::UserOverride.precedence() > TargetSource::Resolved.precedence());
    /// assert!(TargetSource::Resolved.precedence() > TargetSource::Seed.precedence());
    /// ```
    #[must_use]
    pub fn precedence(self) -> u8 {
        match self {
            Self::Seed => 0,
            Self::Resolved => 1,
            Self::UserOverride => 2,
        }
    }

    /// Whether a write with `self` as the incoming source may overwrite an
    /// existing row whose source is `existing`.
    ///
    /// A `user-override` row is sticky: a later `resolved`/`seed` result MUST
    /// NOT overwrite it. An equal-or-higher-precedence incoming source wins
    /// (re-resolving refreshes a `resolved` row; an override always wins).
    ///
    /// This is what [`crate::Cache::upsert`] implementations must honour.
    ///
    /// ```
    /// use simbad_resolver::TargetSource;
    ///
    /// assert!(TargetSource::Resolved.may_overwrite(TargetSource::Seed));
    /// assert!(!TargetSource::Seed.may_overwrite(TargetSource::UserOverride));
    /// ```
    #[must_use]
    pub fn may_overwrite(self, existing: Self) -> bool {
        self.precedence() >= existing.precedence()
    }
}

// ── AliasKind / ResolvedAlias ─────────────────────────────────────────────────

/// The kind of an alias attached to a resolved identity.
///
/// ```
/// use simbad_resolver::AliasKind;
///
/// assert_eq!(AliasKind::CommonName.as_wire(), "common_name");
/// assert_eq!(AliasKind::from_wire("user"), AliasKind::User);
/// assert_eq!(AliasKind::from_wire("bogus"), AliasKind::Designation); // unrecognised → Designation
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AliasKind {
    /// A catalog designation (e.g. `M 31`, `NGC 224`).
    Designation,
    /// A SIMBAD `NAME …` curated common name (e.g. `Andromeda Galaxy`).
    CommonName,
    /// A user-added alias.
    User,
}

impl AliasKind {
    /// The wire/DB string (matches the `target_alias.kind` CHECK constraint).
    ///
    /// ```
    /// use simbad_resolver::AliasKind;
    ///
    /// assert_eq!(AliasKind::Designation.as_wire(), "designation");
    /// ```
    #[must_use]
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Designation => "designation",
            Self::CommonName => "common_name",
            Self::User => "user",
        }
    }

    /// Parse a wire/DB string into an [`AliasKind`]; unknown → `Designation`.
    ///
    /// ```
    /// use simbad_resolver::AliasKind;
    ///
    /// assert_eq!(AliasKind::from_wire("common_name"), AliasKind::CommonName);
    /// ```
    #[must_use]
    pub fn from_wire(s: &str) -> Self {
        match s {
            "common_name" => Self::CommonName,
            "user" => Self::User,
            _ => Self::Designation,
        }
    }
}

/// One alternate designation/name for a resolved identity. The `normalized`
/// form is the typeahead match surface.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResolvedAlias {
    /// Verbatim designation or common name (e.g. `M 31`, `Andromeda Galaxy`).
    pub alias: String,
    /// Normalized form for matching ([`crate::normalize::normalize`]).
    pub normalized: String,
    /// Whether this alias is a designation or a curated common name.
    pub kind: AliasKind,
}

impl ResolvedAlias {
    /// Build a [`ResolvedAlias`], computing the normalized form from `alias`.
    ///
    /// ```
    /// use simbad_resolver::{AliasKind, ResolvedAlias};
    ///
    /// let alias = ResolvedAlias::new("M31", AliasKind::Designation);
    /// assert_eq!(alias.alias, "M31");
    /// assert_eq!(alias.normalized, "m 31");
    /// ```
    #[must_use]
    pub fn new(alias: impl Into<String>, kind: AliasKind) -> Self {
        let alias = alias.into();
        let normalized = crate::normalize::normalize(&alias);
        Self { alias, normalized, kind }
    }
}

// ── ResolvedIdentity / PositionMatch ──────────────────────────────────────────

/// A fully resolved canonical target identity returned by a [`crate::Resolver`].
///
/// Coordinates are ICRS J2000 decimal degrees, sourced from the resolver
/// backend: a resolver that cannot determine a position returns
/// [`crate::ResolveError::NotFound`] rather than a placeholder value.
///
/// ```
/// use simbad_resolver::{AliasKind, ObjectType, ResolvedAlias, ResolvedIdentity, TargetSource};
///
/// let m31 = ResolvedIdentity {
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
///     aliases: vec![ResolvedAlias::new("M 31", AliasKind::Designation)],
///     source: TargetSource::Resolved,
/// };
///
/// // The typed accessor shares one coordinate representation with `skymath`
/// // consumers; `ra_deg`/`dec_deg` remain the source of truth.
/// let eq = m31.position().expect("valid ICRS coordinates");
/// assert!((eq.ra().degrees() - 10.684_708).abs() < 1e-6);
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResolvedIdentity {
    /// SIMBAD physical-object id (the dedup key) when resolved online; `None`
    /// for seed/override-only entries until enriched.
    pub simbad_oid: Option<i64>,
    /// Canonical display designation.
    pub primary_designation: String,
    /// Curated common name (SIMBAD `NAME …`) when one exists.
    pub common_name: Option<String>,
    /// Closed object-type enum from the SIMBAD `otype` mapping ([`map_otype`]).
    pub object_type: ObjectType,
    /// Raw SIMBAD `otype` string, preserved alongside the mapped
    /// [`Self::object_type`] for consumers needing finer-grained types.
    pub otype_raw: String,
    /// ICRS J2000 right ascension in decimal degrees, `[0, 360)`.
    pub ra_deg: f64,
    /// ICRS J2000 declination in decimal degrees, `[-90, 90]`.
    pub dec_deg: f64,
    /// Johnson V-band apparent magnitude (SIMBAD `allfluxes.V`) when the object
    /// has V photometry; `None` otherwise. Many extended/dark objects and
    /// seed/override-only entries have no V magnitude.
    pub v_mag: Option<f64>,
    /// Angular major-axis diameter in arcminutes (SIMBAD `basic.galdim_majaxis`),
    /// unconverted. `None` for a point source (e.g. a star), which genuinely has
    /// no measured angular size — never assume `0.0`.
    pub galdim_majaxis_arcmin: Option<f64>,
    /// Angular minor-axis diameter in arcminutes (SIMBAD `basic.galdim_minaxis`),
    /// unconverted. `None` under the same condition as
    /// [`Self::galdim_majaxis_arcmin`]; a circular object (e.g. a globular
    /// cluster) reports equal major/minor axes rather than omitting the minor
    /// one.
    pub galdim_minaxis_arcmin: Option<f64>,
    /// Position angle of the major axis in degrees (SIMBAD `basic.galdim_angle`),
    /// unconverted. `None` under the same condition as
    /// [`Self::galdim_majaxis_arcmin`].
    pub galdim_angle_deg: Option<i16>,
    /// All designations + common names for this object (the typeahead surface).
    pub aliases: Vec<ResolvedAlias>,
    /// Provenance of this identity.
    pub source: TargetSource,
}

impl ResolvedIdentity {
    /// This target's sky position as a typed `skymath::Equatorial` coordinate.
    ///
    /// SIMBAD TAP `basic.ra`/`basic.dec` are ICRS; at planning grade
    /// (≤ ~1 arcminute) ICRS is treated as J2000 — this encodes that equivalence
    /// once so consumers don't re-derive it. The raw [`Self::ra_deg`] /
    /// [`Self::dec_deg`] fields remain the canonical storage.
    ///
    /// # Errors
    ///
    /// `skymath::Error::OutOfRange` if the stored values are outside RA
    /// `[0, 360)` / Dec `[-90, +90]` (malformed cache content).
    ///
    /// See the [struct-level example](Self) for a full identity literal; this
    /// one only shows the accessor:
    ///
    /// ```
    /// # use simbad_resolver::{AliasKind, ObjectType, ResolvedAlias, ResolvedIdentity, TargetSource};
    /// # let m31 = ResolvedIdentity {
    /// #     simbad_oid: Some(1_575_544), primary_designation: "M 31".to_owned(),
    /// #     common_name: None, object_type: ObjectType::Galaxy, otype_raw: "G".to_owned(),
    /// #     ra_deg: 10.684_708, dec_deg: 41.268_75, v_mag: Some(3.44),
    /// #     galdim_majaxis_arcmin: None, galdim_minaxis_arcmin: None, galdim_angle_deg: None,
    /// #     aliases: vec![ResolvedAlias::new("M 31", AliasKind::Designation)],
    /// #     source: TargetSource::Resolved,
    /// # };
    /// let eq = m31.position()?;
    /// assert!((eq.ra().degrees() - 10.684_708).abs() < 1e-6);
    /// # Ok::<(), skymath::Error>(())
    /// ```
    pub fn position(&self) -> skymath::Result<skymath::Equatorial> {
        skymath::Equatorial::j2000(
            skymath::Angle::from_degrees(self.ra_deg),
            skymath::Angle::from_degrees(self.dec_deg),
        )
    }
}

/// One result of a position (cone-search) resolution.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PositionMatch {
    /// The matched identity.
    pub identity: ResolvedIdentity,
    /// Angular separation from the query position, in degrees.
    pub separation_deg: f64,
}

impl PositionMatch {
    /// This match's sky position as a typed `skymath::Equatorial` coordinate.
    ///
    /// Delegates to [`ResolvedIdentity::position`] on [`Self::identity`].
    ///
    /// # Errors
    ///
    /// `skymath::Error::OutOfRange` if the identity's stored coordinates are out
    /// of range.
    ///
    /// ```
    /// use simbad_resolver::{AliasKind, ObjectType, PositionMatch, ResolvedAlias, ResolvedIdentity, TargetSource};
    ///
    /// # fn run() -> Result<(), skymath::Error> {
    /// let identity = ResolvedIdentity {
    ///     simbad_oid: Some(1_575_544), primary_designation: "M 31".to_owned(),
    ///     common_name: None, object_type: ObjectType::Galaxy, otype_raw: "G".to_owned(),
    ///     ra_deg: 10.684_708, dec_deg: 41.268_75, v_mag: Some(3.44),
    ///     galdim_majaxis_arcmin: None, galdim_minaxis_arcmin: None, galdim_angle_deg: None,
    ///     aliases: vec![ResolvedAlias::new("M 31", AliasKind::Designation)],
    ///     source: TargetSource::Resolved,
    /// };
    /// let m = PositionMatch { identity, separation_deg: 0.001 };
    /// let eq = m.position()?;
    /// assert!((eq.dec().degrees() - 41.268_75).abs() < 1e-6);
    /// # Ok(()) }
    /// ```
    pub fn position(&self) -> skymath::Result<skymath::Equatorial> {
        self.identity.position()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── map_otype ──────────────────────────────────────────────────────────

    #[test]
    fn map_otype_maps_known_codes() {
        assert_eq!(map_otype("G"), ObjectType::Galaxy);
        assert_eq!(map_otype("PN"), ObjectType::PlanetaryNebula);
        assert_eq!(map_otype("HII"), ObjectType::EmissionNebula);
        assert_eq!(map_otype("RNe"), ObjectType::ReflectionNebula);
        assert_eq!(map_otype("DNe"), ObjectType::DarkNebula);
        assert_eq!(map_otype("OpC"), ObjectType::OpenCluster);
        assert_eq!(map_otype("GlC"), ObjectType::GlobularCluster);
        assert_eq!(map_otype("SNR"), ObjectType::SupernovaRemnant);
        assert_eq!(map_otype("ClG"), ObjectType::GalaxyCluster);
        assert_eq!(map_otype("**"), ObjectType::DoubleStar);
        assert_eq!(map_otype("Asterism"), ObjectType::Asterism);
    }

    #[test]
    fn map_otype_covers_full_otype_vocabulary() {
        use ObjectType as OT;
        // Every recognised code + long-form label in the mapping table.
        let cases: &[(&str, ObjectType)] = &[
            ("G", OT::Galaxy),
            ("GiC", OT::Galaxy),
            ("GiG", OT::Galaxy),
            ("GiP", OT::Galaxy),
            ("IG", OT::Galaxy),
            ("PaG", OT::Galaxy),
            ("AGN", OT::Galaxy),
            ("SBG", OT::Galaxy),
            ("rG", OT::Galaxy),
            ("LSB", OT::Galaxy),
            ("AG?", OT::Galaxy),
            ("EmG", OT::Galaxy),
            ("BiC", OT::Galaxy),
            ("H2G", OT::Galaxy),
            ("Sy1", OT::Galaxy),
            ("Sy2", OT::Galaxy),
            ("SyG", OT::Galaxy),
            ("Galaxy", OT::Galaxy),
            ("PN", OT::PlanetaryNebula),
            ("PN?", OT::PlanetaryNebula),
            ("pA*", OT::PlanetaryNebula),
            ("PlanetaryNebula", OT::PlanetaryNebula),
            ("HII", OT::EmissionNebula),
            ("EmO", OT::EmissionNebula),
            ("ISM", OT::EmissionNebula),
            ("RNe?", OT::EmissionNebula),
            ("EmissionNebula", OT::EmissionNebula),
            ("RNe", OT::ReflectionNebula),
            ("ReflectionNebula", OT::ReflectionNebula),
            ("DNe", OT::DarkNebula),
            ("MoC", OT::DarkNebula),
            ("glb", OT::DarkNebula),
            ("cor", OT::DarkNebula),
            ("GNe", OT::DarkNebula),
            ("DarkNebula", OT::DarkNebula),
            ("OpC", OT::OpenCluster),
            ("Cl*", OT::OpenCluster),
            // `As*` is SIMBAD's stellar-association code, intentionally bucketed
            // as an open cluster (not the visual-asterism sense).
            ("As*", OT::OpenCluster),
            ("OpenCluster", OT::OpenCluster),
            ("GlC", OT::GlobularCluster),
            ("GlobularCluster", OT::GlobularCluster),
            ("SNR", OT::SupernovaRemnant),
            ("SNR?", OT::SupernovaRemnant),
            ("SuperNovaRemnant", OT::SupernovaRemnant),
            ("SupernovaRemnant", OT::SupernovaRemnant),
            ("ClG", OT::GalaxyCluster),
            ("GrG", OT::GalaxyCluster),
            ("CGG", OT::GalaxyCluster),
            ("SCG", OT::GalaxyCluster),
            ("GalaxyCluster", OT::GalaxyCluster),
            ("**", OT::DoubleStar),
            ("**?", OT::DoubleStar),
            ("EB*", OT::DoubleStar),
            ("SB*", OT::DoubleStar),
            ("DoubleStar", OT::DoubleStar),
            // Only the long-form label maps to the visual-asterism bucket.
            ("Asterism", OT::Asterism),
        ];
        for (code, want) in cases {
            assert_eq!(map_otype(code), *want, "otype code {code:?} should map to {want:?}");
        }
    }

    #[test]
    fn map_otype_unknown_is_other() {
        assert_eq!(map_otype("ZZZ"), ObjectType::Other);
        assert_eq!(map_otype(""), ObjectType::Other);
        assert_eq!(map_otype("Star"), ObjectType::Other);
    }

    #[test]
    fn map_otype_trims_whitespace() {
        assert_eq!(map_otype("  G  "), ObjectType::Galaxy);
    }

    #[test]
    fn object_type_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&ObjectType::PlanetaryNebula).unwrap(),
            "\"planetary_nebula\""
        );
        assert_eq!(
            serde_json::to_string(&ObjectType::GalaxyCluster).unwrap(),
            "\"galaxy_cluster\""
        );
        assert_eq!(serde_json::to_string(&ObjectType::Other).unwrap(), "\"other\"");
    }

    #[test]
    fn object_type_wire_round_trips() {
        for ot in [
            ObjectType::Galaxy,
            ObjectType::PlanetaryNebula,
            ObjectType::EmissionNebula,
            ObjectType::ReflectionNebula,
            ObjectType::DarkNebula,
            ObjectType::OpenCluster,
            ObjectType::GlobularCluster,
            ObjectType::SupernovaRemnant,
            ObjectType::GalaxyCluster,
            ObjectType::DoubleStar,
            ObjectType::Asterism,
            ObjectType::Other,
        ] {
            assert_eq!(ObjectType::from_wire(ot.as_wire()), ot);
        }
        assert_eq!(ObjectType::from_wire("bogus"), ObjectType::Other);
    }

    // ── TargetSource ─────────────────────────────────────────────────────────

    #[test]
    fn target_source_user_override_is_hyphenated() {
        assert_eq!(
            serde_json::to_string(&TargetSource::UserOverride).unwrap(),
            "\"user-override\""
        );
        assert_eq!(serde_json::to_string(&TargetSource::Seed).unwrap(), "\"seed\"");
        assert_eq!(serde_json::to_string(&TargetSource::Resolved).unwrap(), "\"resolved\"");
        assert_eq!(
            serde_json::from_str::<TargetSource>("\"user-override\"").unwrap(),
            TargetSource::UserOverride
        );
    }

    #[test]
    fn target_source_wire_round_trips() {
        for src in [TargetSource::Seed, TargetSource::Resolved, TargetSource::UserOverride] {
            assert_eq!(TargetSource::from_wire(src.as_wire()), Some(src));
        }
        assert_eq!(TargetSource::from_wire("bogus"), None);
    }

    #[test]
    fn target_source_precedence_and_overwrite() {
        assert!(TargetSource::Seed.precedence() < TargetSource::Resolved.precedence());
        assert!(TargetSource::Resolved.precedence() < TargetSource::UserOverride.precedence());

        // Equal-or-higher precedence may overwrite.
        assert!(TargetSource::Resolved.may_overwrite(TargetSource::Seed));
        assert!(TargetSource::Resolved.may_overwrite(TargetSource::Resolved));
        assert!(TargetSource::UserOverride.may_overwrite(TargetSource::Resolved));

        // A user-override row is sticky: lower-precedence incoming sources
        // must not overwrite it.
        assert!(!TargetSource::Seed.may_overwrite(TargetSource::UserOverride));
        assert!(!TargetSource::Resolved.may_overwrite(TargetSource::UserOverride));
    }

    // ── AliasKind / ResolvedAlias ──────────────────────────────────────────

    #[test]
    fn alias_kind_wire_round_trips() {
        for kind in [AliasKind::Designation, AliasKind::CommonName, AliasKind::User] {
            assert_eq!(AliasKind::from_wire(kind.as_wire()), kind);
        }
        assert_eq!(AliasKind::from_wire("bogus"), AliasKind::Designation);
    }

    #[test]
    fn resolved_alias_computes_normalized_form() {
        let alias = ResolvedAlias::new("M31", AliasKind::Designation);
        assert_eq!(alias.alias, "M31");
        assert_eq!(alias.normalized, "m 31");
    }
}
