//! The diagnostic catalogue, and the two commands that read it.
//!
//! `docs/spec/v1/*.md` is the definition of every code. `build.rs`
//! extracts the rows into the table below rather than anyone maintaining
//! a second list: two lists means one of them is wrong, and it is always
//! the one nobody reads.

include!(concat!(env!("OUT_DIR"), "/diagnostic_catalogue.rs"));

/// One row, or `None` for a code the spec does not document.
///
/// Case-insensitive on the letter so `jwc lint --explain e0211` works;
/// the codes themselves are uppercase everywhere else.
pub fn lookup(code: &str) -> Option<(&'static str, &'static str, &'static str)> {
    let want = code.trim().to_ascii_uppercase();
    DIAGNOSTIC_CATALOGUE
        .iter()
        .find(|(c, _, _)| *c == want)
        .copied()
}

/// Codes whose first two digits match, for the "did you mean" list when a
/// lookup misses. `E02xx` is names, `E03xx` types, `E07xx` routing — so
/// the band is a useful answer even when the exact code is a typo.
pub fn in_same_band(code: &str) -> Vec<(&'static str, &'static str, &'static str)> {
    let want = code.trim().to_ascii_uppercase();
    if want.len() < 3 {
        return Vec::new();
    }
    let band = &want[..3];
    DIAGNOSTIC_CATALOGUE
        .iter()
        .filter(|(c, _, _)| c.starts_with(band))
        .copied()
        .collect()
}
