// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Pure TSV/wire parsing helpers for a SIMBAD TAP
//! `basic ⋈ allfluxes` row.
//!
//! This crate stays free of a `csv`-crate tokenizer, so [`parse_basic_row`]
//! splits on the tab delimiter directly. SIMBAD's TAP `format=tsv` output
//! never quotes a tab inside a string column, so a plain `split('\t')` is
//! equivalent to the RFC-4180 reader for this fixed 9-column shape (`oid,
//! main_id, ra, dec, otype_txt, V, galdim_majaxis, galdim_minaxis,
//! galdim_angle`).

/// Strip SIMBAD's surrounding double quotes (TSV string columns are quoted)
/// and outer whitespace.
///
/// ```
/// use simbad_resolver::wire::unquote;
///
/// assert_eq!(unquote("\"M 31\""), "M 31");
/// assert_eq!(unquote("  12345  "), "12345");
/// ```
#[must_use]
pub fn unquote(s: &str) -> String {
    s.trim().trim_matches('"').to_owned()
}

/// Collapse internal whitespace runs to single spaces and trim
/// (e.g. SIMBAD `"M   31"` → `"M 31"`, `"NGC  224"` → `"NGC 224"`).
///
/// ```
/// use simbad_resolver::wire::collapse_spaces;
///
/// assert_eq!(collapse_spaces("M   31"), "M 31");
/// assert_eq!(collapse_spaces("  NGC  224 "), "NGC 224");
/// ```
#[must_use]
pub fn collapse_spaces(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parse a `basic ⋈ allfluxes` TSV line into
/// `(oid, main_id, ra, dec, otype, v_mag, galdim_majaxis, galdim_minaxis,
/// galdim_angle)`.
///
/// RA/Dec are range-validated (ICRS J2000: `ra ∈ [0, 360)`, `dec ∈ [-90, 90]`)
/// so an out-of-range or non-finite value degrades to `None` (a no-result row)
/// rather than reaching a caller's storage layer with an invalid coordinate.
///
/// The trailing V, galdim_majaxis, galdim_minaxis, and galdim_angle columns are
/// all optional: SIMBAD emits an empty field for an object with no V
/// photometry (a `LEFT OUTER JOIN allfluxes` miss) or no angular-size
/// measurement (e.g. a point source, `* alf Lyr`), which parses to `None` —
/// never `0.0`/`0`, since "no measurement" and "measured as zero" are
/// different facts. A non-empty but unparsable/non-finite value in any of
/// these columns also degrades to `None` rather than dropping the whole row.
///
/// ```
/// use simbad_resolver::wire::parse_basic_row;
///
/// let line = "1575544\t\"M  31\"\t10.6847083\t41.26875\t\"G\"\t3.44\t199.53\t70.79\t35";
/// let (oid, main_id, ra, dec, otype, v_mag, majaxis, minaxis, angle) =
///     parse_basic_row(line).unwrap();
/// assert_eq!(oid, 1_575_544);
/// assert_eq!(main_id, "M  31"); // internal whitespace is not collapsed here
/// assert_eq!(otype, "G");
/// assert_eq!(v_mag, Some(3.44));
/// assert_eq!(majaxis, Some(199.53));
/// assert_eq!(minaxis, Some(70.79));
/// assert_eq!(angle, Some(35));
/// # let _ = (ra, dec);
/// ```
#[must_use]
#[allow(clippy::type_complexity)]
pub fn parse_basic_row(
    line: &str,
) -> Option<(i64, String, f64, f64, String, Option<f64>, Option<f64>, Option<f64>, Option<i16>)> {
    let fields: Vec<&str> = line.split('\t').collect();
    if fields.len() < 9 {
        return None;
    }
    let oid: i64 = unquote(fields[0]).parse().ok()?;
    let main_id = unquote(fields[1]);
    let ra: f64 = unquote(fields[2]).parse().ok()?;
    let dec: f64 = unquote(fields[3]).parse().ok()?;
    let otype = unquote(fields[4]);
    let v_mag = parse_optional_mag(fields[5]);
    let galdim_majaxis = parse_optional_mag(fields[6]);
    let galdim_minaxis = parse_optional_mag(fields[7]);
    let galdim_angle = parse_optional_i16(fields[8]);

    if !ra.is_finite() || !(0.0..360.0).contains(&ra) {
        return None;
    }
    if !dec.is_finite() || !(-90.0..=90.0).contains(&dec) {
        return None;
    }
    Some((oid, main_id, ra, dec, otype, v_mag, galdim_majaxis, galdim_minaxis, galdim_angle))
}

/// Parse an optional magnitude TSV field: empty → `None`; otherwise a finite
/// `f64` → `Some`, or `None` if unparsable/non-finite.
///
/// ```
/// use simbad_resolver::wire::parse_optional_mag;
///
/// assert_eq!(parse_optional_mag("3.44"), Some(3.44));
/// assert_eq!(parse_optional_mag(""), None); // LEFT OUTER JOIN miss: no V photometry
/// assert_eq!(parse_optional_mag("~"), None); // unparsable degrades to None, not an error
/// ```
#[must_use]
pub fn parse_optional_mag(field: &str) -> Option<f64> {
    let s = unquote(field);
    if s.is_empty() {
        return None;
    }
    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Parse an optional SHORT-encoded TSV field (`galdim_angle`, a whole-degree
/// position angle): empty → `None`; otherwise a valid `i16` → `Some`, or
/// `None` if unparsable/out of range.
///
/// ```
/// use simbad_resolver::wire::parse_optional_i16;
///
/// assert_eq!(parse_optional_i16("35"), Some(35));
/// assert_eq!(parse_optional_i16(""), None); // no galdim measurement (e.g. a point source)
/// assert_eq!(parse_optional_i16("~"), None); // unparsable degrades to None, not an error
/// ```
#[must_use]
pub fn parse_optional_i16(field: &str) -> Option<i16> {
    let s = unquote(field);
    if s.is_empty() {
        return None;
    }
    s.parse::<i16>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unquote_strips_tsv_quotes() {
        assert_eq!(unquote("\"M 31\""), "M 31");
        assert_eq!(unquote("  12345  "), "12345");
    }

    #[test]
    fn collapse_spaces_normalizes_padding() {
        assert_eq!(collapse_spaces("M   31"), "M 31");
        assert_eq!(collapse_spaces("  NGC  224 "), "NGC 224");
    }

    #[test]
    fn parse_basic_row_extracts_columns() {
        let line = "1575544\t\"M  31\"\t10.6847083\t41.26875\t\"G\"\t3.44\t199.53\t70.79\t35";
        let (oid, main_id, ra, dec, otype, v_mag, majaxis, minaxis, angle) =
            parse_basic_row(line).unwrap();
        assert_eq!(oid, 1_575_544);
        assert_eq!(main_id, "M  31");
        assert!((ra - 10.684_708_3).abs() < 1e-6);
        assert!((dec - 41.268_75).abs() < 1e-6);
        assert_eq!(otype, "G");
        assert_eq!(v_mag, Some(3.44));
        assert_eq!(majaxis, Some(199.53));
        assert_eq!(minaxis, Some(70.79));
        assert_eq!(angle, Some(35));
    }

    #[test]
    fn parse_basic_row_equal_axes_object() {
        // M 57: a near-circular object reports equal major/minor axes rather
        // than omitting the minor one.
        let line = "1560141\t\"M  57\"\t283.3963\t33.0292\t\"PN\"\t8.79\t1.153\t1.153\t90";
        let (.., majaxis, minaxis, angle) = parse_basic_row(line).unwrap();
        assert_eq!(majaxis, Some(1.153));
        assert_eq!(minaxis, Some(1.153));
        assert_eq!(angle, Some(90));
    }

    #[test]
    fn parse_basic_row_point_source_has_no_galdim() {
        // Vega: a point source has no angular-size measurement at all — this
        // must read back as None, never 0.0/0 (a genuinely different fact).
        let line = "3672809\t\"* alf Lyr\"\t279.2347\t38.7837\t\"*\"\t0.03\t\t\t";
        let (.., majaxis, minaxis, angle) = parse_basic_row(line).unwrap();
        assert_eq!(majaxis, None);
        assert_eq!(minaxis, None);
        assert_eq!(angle, None);
    }

    #[test]
    fn parse_basic_row_empty_v_is_none() {
        // A LEFT OUTER JOIN miss yields an empty trailing field, not a dropped row.
        let line = "1575544\t\"M  31\"\t10.6847083\t41.26875\t\"G\"\t\t\t\t";
        let (.., v_mag, majaxis, minaxis, angle) = parse_basic_row(line).unwrap();
        assert_eq!(v_mag, None);
        assert_eq!(majaxis, None);
        assert_eq!(minaxis, None);
        assert_eq!(angle, None);
    }

    #[test]
    fn parse_basic_row_unparsable_v_is_none_not_dropped() {
        let line = "1575544\t\"M  31\"\t10.6847083\t41.26875\t\"G\"\t~\t~\t~\t~";
        let row = parse_basic_row(line).unwrap();
        assert_eq!(row.5, None);
        assert_eq!(row.6, None);
        assert_eq!(row.7, None);
        assert_eq!(row.8, None);
    }

    #[test]
    fn parse_basic_row_rejects_short_lines() {
        assert!(parse_basic_row("1\t2\t3").is_none());
        // Eight columns (pre-galdim shape) is now too short.
        assert!(parse_basic_row("1\t\"X\"\t10.0\t41.0\t\"G\"\t3.4\t1.0\t1.0").is_none());
    }

    #[test]
    fn parse_basic_row_rejects_non_numeric_fields() {
        assert!(parse_basic_row("abc\t\"M 31\"\t10.0\t41.0\t\"G\"\t3.4\t\t\t").is_none());
    }

    #[test]
    fn parse_basic_row_rejects_ra_out_of_range() {
        // ra must be in [0, 360); 360.0 and negative values are rejected.
        assert!(parse_basic_row("1\t\"X\"\t360.0\t0.0\t\"G\"\t3.4\t\t\t").is_none());
        assert!(parse_basic_row("1\t\"X\"\t-1.0\t0.0\t\"G\"\t3.4\t\t\t").is_none());
    }

    #[test]
    fn parse_basic_row_rejects_dec_out_of_range() {
        // dec must be in [-90, 90]; 90.0/-90.0 are valid boundaries.
        assert!(parse_basic_row("1\t\"X\"\t10.0\t90.1\t\"G\"\t3.4\t\t\t").is_none());
        assert!(parse_basic_row("1\t\"X\"\t10.0\t-90.1\t\"G\"\t3.4\t\t\t").is_none());
        assert!(parse_basic_row("1\t\"X\"\t10.0\t90.0\t\"G\"\t3.4\t\t\t").is_some());
    }

    #[test]
    fn parse_basic_row_rejects_non_finite_coordinates() {
        assert!(parse_basic_row("1\t\"X\"\tnan\t0.0\t\"G\"\t3.4\t\t\t").is_none());
        assert!(parse_basic_row("1\t\"X\"\t10.0\tinf\t\"G\"\t3.4\t\t\t").is_none());
    }

    #[test]
    fn parse_optional_i16_parses_degrees() {
        assert_eq!(parse_optional_i16("35"), Some(35));
        assert_eq!(parse_optional_i16("-10"), Some(-10));
        assert_eq!(parse_optional_i16(""), None);
        assert_eq!(parse_optional_i16("~"), None);
        assert_eq!(parse_optional_i16("99999"), None, "out of i16 range degrades to None");
    }
}
