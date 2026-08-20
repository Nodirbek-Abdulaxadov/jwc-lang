//! The sample's snapshot, as a checked-in file.
//!
//! `docs/spec/v1/sample/` is a complete application — 13 tables, 5 views,
//! every constraint class the language has. Freezing its snapshot is how a
//! change to what the snapshot records shows up as a reviewable diff rather
//! than as a silently different migration on someone's next `migrate new`.
//!
//! Regenerate with:
//!
//! ```bash
//! JWC_BLESS=1 cargo test --test snapshot_sample
//! ```

use jwc::{model, snapshot, workspace::Workspace};
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn the_sample_snapshot_is_frozen() {
    let root = repo_root();
    let ws = Workspace::load(root.join("docs/spec/v1/sample")).expect("load sample");
    assert!(!ws.has_parse_errors(), "{}", ws.parse_errors().join(""));
    let built = model::build(&ws);
    let text = snapshot::of(&built.model).to_json();

    let golden = root.join("tests/snapshot_sample.json");
    if std::env::var("JWC_BLESS").is_ok() {
        std::fs::write(&golden, &text).expect("write golden");
        return;
    }
    let want = std::fs::read_to_string(&golden).unwrap_or_default();
    assert_eq!(
        text,
        want,
        "the sample's snapshot changed. Review the diff, then re-bless with \
         JWC_BLESS=1 cargo test --test snapshot_sample"
    );

    // Reading the file back is the property `migrate new` depends on: it
    // reads the previous state from JSON it wrote, and any field that
    // serialises but does not deserialise would be silently lost there
    // rather than here.
    let snap = snapshot::Snapshot::from_json(&want).expect("re-read");
    assert_eq!(snap.to_json(), want, "snapshot is not a fixed point");
    // A floor, not a target: it exists so an empty or truncated golden file
    // reads as a failure rather than as "nothing to check".
    assert!(snap.tables.len() >= 13, "tables: {}", snap.tables.len());
    assert!(snap.views.len() >= 5, "views: {}", snap.views.len());
}
