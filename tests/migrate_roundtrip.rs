//! The round-trip property: **a database you migrated into a shape is the
//! same database as one created in that shape.**
//!
//! This is v0.26.0's acceptance test (ROADMAP §10). A random walk of schema
//! edits is applied one migration at a time to a database, a second
//! database is built by applying `gen-sql` of the *final* source to an empty
//! one, and the two are compared with `pg_dump --schema-only`.
//!
//! Set `JWC_V1_DATABASE_URL` (a database this may drop and recreate schemas
//! in) or it prints SKIPPED — and **a SKIPPED line is not a pass**. This
//! test is the only thing in the repository that can tell a migration that
//! *reads* right from one that *is* right.
//!
//! `JWC_ROUNDTRIP_SEQUENCES` sets how many walks to run (default 20, which
//! is the per-change number in migrations.md §10.2; 200 is the full run).
//!
//! ## What is normalised, and why
//!
//! Only four things, each because no migration can control it:
//!
//! * `\restrict` / `\unrestrict` carry a random nonce per `pg_dump` run;
//! * **column order** — Postgres appends, so a column written into the
//!   middle of a declaration sits at the end of a migrated table and in the
//!   middle of a freshly created one. Nothing depends on the order, and no
//!   `ALTER` can change it;
//! * **statement order** — `pg_dump` emits objects by OID, which is creation
//!   order: the order the migrations ran on one side and declaration order
//!   on the other. Statements are compared as a sorted set;
//! * an identity column's **sequence name**, which Postgres does not change
//!   when the table is renamed.
//!
//! Everything else — types, nullability, defaults, identity, every
//! constraint and index with its generated name and canonical predicate,
//! enum members *in order*, view bodies, triggers, comments — is compared
//! literally.

use jwc::{apply, ddl, migrate, model, snapshot, workspace::Workspace};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::path::{Path, PathBuf};
use std::process::Command;
use tokio_postgres::Client;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

// ── a schema that can be edited and printed ────────────────────────────

#[derive(Clone)]
struct Col {
    name: String,
    width: u32,
    was: Option<String>,
}

#[derive(Clone)]
struct Extra {
    name: String,
    was: Option<String>,
    label_width: u32,
}

#[derive(Clone)]
struct Gen {
    /// Appended enum members. `free` and `pro` are always there.
    enum_extra: Vec<String>,
    cols: Vec<Col>,
    indexed: Vec<String>,
    /// `None`, or the predicate of the partial unique on `region`. A
    /// predicated unique is an *index* (schema.md §4.3); the plain one
    /// below is a table constraint, and the two travel different phases.
    unique_pred: Option<&'static str>,
    /// A plain `unique (c)` on a generated column, or none.
    uniqued: Option<String>,
    projected: Vec<String>,
    extras: Vec<Extra>,
    comment: &'static str,
    counter: usize,
}

impl Gen {
    fn new() -> Gen {
        Gen {
            enum_extra: Vec::new(),
            // One generated column from the start, so a widening is
            // reachable on step 1 rather than only after an add.
            cols: vec![Col {
                name: "c0".into(),
                width: 20,
                was: None,
            }],
            indexed: Vec::new(),
            unique_pred: None,
            uniqued: None,
            projected: Vec::new(),
            // One from the start, so renaming and dropping a table are both
            // reachable on the first step.
            extras: vec![Extra {
                name: "T0".into(),
                was: None,
                label_width: 30,
            }],
            comment: "Tenants.",
            counter: 0,
        }
    }

    fn source(&self) -> String {
        let mut s = String::from(
            "namespace r;\ndatabase App : Postgres;\nschema org of App;\n\n",
        );
        let members: Vec<String> = ["free".to_string(), "pro".to_string()]
            .into_iter()
            .chain(self.enum_extra.iter().cloned())
            .collect();
        s.push_str(&format!(
            "enum Plan of App.org {{ {} }}\n\n",
            members.join(", ")
        ));
        s.push_str(&format!("--- {}\ntable Orgs of App.org {{\n", self.comment));
        s.push_str("    id         bigint primary key identity;\n");
        s.push_str("    slug       varchar(40) unique;\n");
        s.push_str("    plan       Plan;\n");
        s.push_str("    region     varchar(20)?;\n");
        s.push_str("    retired_at timestamptz?;\n");
        for c in &self.cols {
            let was = c
                .was
                .as_ref()
                .map(|w| format!(" was \"{w}\""))
                .unwrap_or_default();
            s.push_str(&format!(
                "    {} varchar({})?{was};\n",
                c.name, c.width
            ));
        }
        if let Some(p) = self.unique_pred {
            s.push_str(&format!(
                "\n    unique (region) where {p} : \"bitta hudud\";\n"
            ));
        }
        if let Some(c) = &self.uniqued {
            s.push_str(&format!("    unique ({c}) : \"bitta qiymat\";\n"));
        }
        for i in &self.indexed {
            s.push_str(&format!("    index on ({i});\n"));
        }
        s.push_str("}\n\n");

        for e in &self.extras {
            let was = e
                .was
                .as_ref()
                .map(|w| format!(" was \"{w}\""))
                .unwrap_or_default();
            s.push_str(&format!(
                "table {} of App.org{was} {{\n    \
                 id    bigint primary key identity;\n    \
                 label varchar({})?;\n}}\n\n",
                e.name, e.label_width
            ));
        }

        let fields: Vec<String> = ["id".to_string(), "slug".to_string()]
            .into_iter()
            .chain(self.projected.iter().cloned())
            .collect();
        s.push_str(&format!(
            "view OrgSummary of App.org {{\n    select O from App.org.Orgs as {{ {} }};\n}}\n",
            fields.join(", ")
        ));
        s
    }

    /// One edit. `was` markers live for exactly one migration
    /// (migrations.md §6.4), so every step clears the previous ones first.
    ///
    /// `forced` pins which edit to make. The walk uses it for the first two
    /// steps of every sequence, cycling through the whole edit vocabulary as
    /// the seed advances — so a run of a dozen sequences reaches every
    /// operation class instead of leaving the tail to luck. Two steps rather
    /// than one because the toggles (a unique, an index, the projection)
    /// only reach their `drop` half on the second.
    fn step(&mut self, rng: &mut StdRng, forced: Option<u32>) {
        for c in &mut self.cols {
            c.was = None;
        }
        for e in &mut self.extras {
            e.was = None;
        }
        self.counter += 1;
        let n = self.counter;

        // Only columns nothing else names are safe to drop or rename: a
        // projected one would leave the view selecting a column that is not
        // there, which is a broken *program*, not a migration to test.
        let free: Vec<usize> = (0..self.cols.len())
            .filter(|i| {
                let c = &self.cols[*i];
                !self.projected.contains(&c.name)
                    && !self.indexed.contains(&c.name)
                    && self.uniqued.as_deref() != Some(c.name.as_str())
            })
            .collect();

        match forced.unwrap_or_else(|| rng.gen_range(0..12)) {
            0 => self.cols.push(Col {
                name: format!("c{n}"),
                width: 20 + (n as u32 % 5) * 10,
                was: None,
            }),
            1 if !free.is_empty() => {
                let i = free[rng.gen_range(0..free.len())];
                self.cols.remove(i);
            }
            2 if !free.is_empty() => {
                let i = free[rng.gen_range(0..free.len())];
                let old = self.cols[i].name.clone();
                self.cols[i].name = format!("r{n}");
                self.cols[i].was = Some(old);
            }
            3 if !self.cols.is_empty() => {
                let i = rng.gen_range(0..self.cols.len());
                self.cols[i].width += 20;
            }
            4 => {
                self.unique_pred = match rng.gen_range(0..3) {
                    0 => None,
                    1 => Some("retired_at == null"),
                    _ => Some("retired_at != null"),
                }
            }
            5 => self.enum_extra.push(format!("v{n}")),
            6 => {
                // The view's projection: `region` is always there to add,
                // and a projected column can be dropped from it again.
                if self.projected.contains(&"region".to_string()) {
                    self.projected.retain(|x| x != "region");
                } else {
                    self.projected.push("region".to_string());
                }
            }
            7 => self.extras.push(Extra {
                name: format!("T{n}"),
                was: None,
                label_width: 30,
            }),
            8 if !self.extras.is_empty() => {
                let i = rng.gen_range(0..self.extras.len());
                if rng.gen_bool(0.5) {
                    self.extras.remove(i);
                } else {
                    let old = self.extras[i].name.clone();
                    self.extras[i].name = format!("T{n}r");
                    self.extras[i].was = Some(crate::physical(&old));
                }
            }
            9 => {
                if self.indexed.is_empty() {
                    self.indexed.push("region".to_string());
                } else {
                    self.indexed.clear();
                }
            }
            10 => {
                // A plain unique is a table constraint, so this is the arm
                // that reaches phase 4 and phase 9's `DROP CONSTRAINT`.
                self.uniqued = match &self.uniqued {
                    Some(_) => None,
                    None => self.cols.first().map(|c| c.name.clone()),
                };
            }
            11 if !self.cols.is_empty() => {
                let i = rng.gen_range(0..self.cols.len());
                self.cols[i].width += 20;
            }
            _ => {
                self.comment = if self.comment == "Tenants." {
                    "Tenants, one per customer."
                } else {
                    "Tenants."
                }
            }
        }
    }
}

/// `was "…"` names the *physical* name, which for a table is snake_case.
fn physical(declared: &str) -> String {
    jwc::naming::physical(declared)
}

// ── the harness ────────────────────────────────────────────────────────

fn model_of(text: &str, dir: &Path) -> model::SchemaModel {
    let p = dir.join("a.jwc");
    std::fs::write(&p, text).expect("write");
    let ws = Workspace::load(&p).expect("load");
    assert!(
        !ws.has_parse_errors(),
        "generated source did not parse:\n{}\n{}",
        ws.parse_errors().join(""),
        text
    );
    let built = model::build(&ws);
    let errors: Vec<String> = built
        .diags
        .iter()
        .filter(|(_, d)| d.severity == jwc::diag::Severity::Error)
        .map(|(loc, d)| ws.render(*loc, d))
        .collect();
    assert!(
        errors.is_empty(),
        "generated source has errors:\n{}\n{text}",
        errors.join("")
    );
    built.model
}

fn full_script(m: &model::SchemaModel) -> String {
    let ws = Workspace {
        root: PathBuf::new(),
        files: Vec::new(),
        packages: Default::default(),
        manifest: None,
    };
    ddl::render(&ws, &ddl::emit(m), false)
}

async fn connect(url: &str) -> Client {
    jwc::engine::connect_for_migrations(url)
        .await
        .expect("connect")
}

async fn wipe(client: &Client) {
    client
        .batch_execute(
            "DROP SCHEMA IF EXISTS org CASCADE;
             DROP TABLE IF EXISTS public._jwc_migrations;",
        )
        .await
        .expect("wipe");
}

fn dump(url: &str) -> String {
    let out = Command::new("pg_dump")
        .args([
            "--schema-only",
            "--no-owner",
            "--no-privileges",
            "-n",
            "org",
            "-d",
            url,
        ])
        .output()
        .expect("pg_dump");
    assert!(
        out.status.success(),
        "pg_dump failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    normalise(&String::from_utf8_lossy(&out.stdout))
}

/// See the module note: four normalisations, each for something no
/// migration can control.
fn normalise(text: &str) -> String {
    let mut statements: Vec<String> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut in_body = false;

    for line in text.lines() {
        let t = line.trim();
        if !in_body
            && (t.is_empty()
                || t.starts_with("--")
                || t.starts_with("\\restrict")
                || t.starts_with("\\unrestrict"))
        {
            continue;
        }
        // An identity column's sequence keeps its name when the table is
        // renamed. The name is Postgres's, not the schema's.
        let t = if t.starts_with("SEQUENCE NAME ") {
            "SEQUENCE NAME <generated>"
        } else {
            t
        };
        if !t.matches("$$").count().is_multiple_of(2) {
            in_body = !in_body;
        }
        current.push(t.to_string());
        if !in_body && t.ends_with(';') {
            statements.push(finish(std::mem::take(&mut current)));
        }
    }
    if !current.is_empty() {
        statements.push(finish(current));
    }

    // `pg_dump` emits objects in creation order, which for a migrated
    // database is the order the migrations ran and for a fresh one is
    // declaration order. Two databases holding the same objects are the
    // same database; the OIDs are history, not schema.
    statements.sort();
    statements.join("\n")
}

/// Postgres appends columns; a fresh `CREATE TABLE` writes them in
/// declaration order. Nothing depends on the order and no `ALTER` can
/// change it, so the two are compared as sets.
fn finish(mut lines: Vec<String>) -> String {
    if lines.len() > 2 && lines[0].starts_with("CREATE TABLE ") && lines[0].ends_with('(') {
        let last = lines.len() - 1;
        let mut cols: Vec<String> = lines[1..last]
            .iter()
            .map(|l| l.trim_end_matches(',').to_string())
            .collect();
        cols.sort();
        let mut out = vec![lines[0].clone()];
        out.extend(cols);
        out.push(lines[last].clone());
        lines = out;
    }
    lines.join("\n")
}

/// Walk `steps` random edits, migrating one at a time, and prove the result
/// is the same database as a fresh `gen-sql` of where it ended up.
async fn one_sequence(
    seed: u64,
    steps: usize,
    url_a: &str,
    url_b: &str,
    seen: &mut std::collections::BTreeSet<String>,
) {
    let a = connect(url_a).await;
    let b = connect(url_b).await;
    wipe(&a).await;
    wipe(&b).await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("migrations");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let src = tempfile::tempdir().expect("tempdir");

    let mut rng = StdRng::seed_from_u64(seed);
    let mut g = Gen::new();
    let mut prev = snapshot::Snapshot::default();
    let mut last_source = g.source();

    for step in 0..=steps {
        if step > 0 {
            let forced = (step <= 2).then_some(seed as u32 % 12);
            g.step(&mut rng, forced);
            last_source = g.source();
        }
        let m = model_of(&last_source, src.path());
        let ordinal = snapshot::next_ordinal(&dir);
        let plan = migrate::plan(&prev, &m, ordinal, &format!("s{step}"));
        assert!(
            !plan.has_errors(),
            "seed {seed} step {step}: the plan was refused\n{last_source}"
        );
        for e in &plan.explain {
            seen.insert(e.text.split_whitespace().next().unwrap_or("").to_string());
        }
        for f in &plan.files {
            std::fs::write(dir.join(format!("{}.up.sql", f.stem)), &f.up).expect("write");
            std::fs::write(dir.join(format!("{}.down.sql", f.stem)), &f.down).expect("write");
            if let Some(s) = &f.snapshot {
                std::fs::write(dir.join(format!("{}.snapshot.json", f.stem)), s)
                    .expect("write");
                prev = snapshot::Snapshot::from_json(s).expect("re-read");
            }
        }
        apply::up(&a, &dir, None)
            .await
            .unwrap_or_else(|e| panic!("seed {seed} step {step}: {e:#}\n{last_source}"));
    }

    // The other side: the same final source, applied to an empty database.
    let m = model_of(&last_source, src.path());
    b.batch_execute(&full_script(&m))
        .await
        .unwrap_or_else(|e| panic!("seed {seed}: gen-sql did not apply: {e:#}\n{last_source}"));

    let (x, y) = (dump(url_a), dump(url_b));
    if x != y {
        let diff: Vec<String> = x
            .lines()
            .zip(y.lines())
            .filter(|(a, b)| a != b)
            .map(|(a, b)| format!("  migrated: {a}\n  fresh:    {b}"))
            .collect();
        panic!(
            "seed {seed}: a migrated database is not the database `gen-sql` builds\n\
             {}\n\nsource:\n{last_source}",
            if diff.is_empty() {
                format!("(different lengths: {} vs {})", x.lines().count(), y.lines().count())
            } else {
                diff.join("\n")
            }
        );
    }
}

/// Two scratch databases beside the one named, both dropped and
/// recreated.
///
/// `tag` keeps two tests in this file off each other. They used to share
/// `_rt_a` / `_rt_b`, and cargo runs the tests inside one binary in
/// parallel — so whichever got there second dropped a database the first
/// was already migrating, and the failure surfaced as a `pg_database`
/// unique violation naming neither test.
fn databases(tag: &str) -> Option<(String, String)> {
    let base = std::env::var("JWC_V1_DATABASE_URL").ok()?;
    let (head, name) = base.rsplit_once('/')?;
    let a = format!("{head}/{name}_{tag}_a");
    let bb = format!("{head}/{name}_{tag}_b");
    for db in [&a, &bb] {
        let dbname = db.rsplit('/').next()?;
        let out = Command::new("psql")
            .args([
                &base,
                "-q",
                "-c",
                &format!("DROP DATABASE IF EXISTS {dbname}"),
                "-c",
                &format!("CREATE DATABASE {dbname}"),
            ])
            .output()
            .expect("psql");
        assert!(
            out.status.success(),
            "could not create {dbname}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Some((a, bb))
}

#[tokio::test]
async fn a_migrated_database_is_a_created_database() {
    let Some((a, b)) = databases("rt") else {
        eprintln!(
            "SKIPPED a_migrated_database_is_a_created_database — set \
             JWC_V1_DATABASE_URL. A SKIPPED line is not a pass: this is the \
             only test that can tell a migration that reads right from one \
             that is right."
        );
        return;
    };
    let n: u64 = std::env::var("JWC_ROUNDTRIP_SEQUENCES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let mut seen = std::collections::BTreeSet::new();
    for seed in 0..n {
        one_sequence(seed, 8, &a, &b, &mut seen).await;
    }
    eprintln!(
        "{n} random sequences, 8 edits each — operations exercised: {}",
        seen.iter().cloned().collect::<Vec<_>>().join(", ")
    );

    // A green run is only evidence if the walk actually walked. Without
    // this, a generator that stopped producing edits would read as a pass —
    // the same shape of mistake as an EXPLAIN assertion that holds either
    // way.
    for kind in [
        "add_column",
        "drop_column",
        "rename_column",
        "alter_column_type",
        "add_enum_value",
        "create_index",
        "drop_index",
        "create_table",
        "drop_table",
        "rename_table",
        "create_view",
        "drop_view",
        "add_constraint",
        "drop_constraint",
        "comment_on",
    ] {
        assert!(
            seen.contains(kind),
            "the random walk never produced `{kind}` — it is not testing what it claims"
        );
    }
}

/// The case ROADMAP §10 names: widening a column that sits underneath a
/// view. The view blocks the `ALTER`, so it has to come down in phase 0 and
/// go back up in phase 8 — and this is the real sample, not a fixture built
/// to make that easy.
#[tokio::test]
async fn widening_a_column_under_a_view_applies() {
    let Some((a, _b)) = databases("view") else {
        eprintln!(
            "SKIPPED widening_a_column_under_a_view_applies — set \
             JWC_V1_DATABASE_URL. A SKIPPED line is not a pass."
        );
        return;
    };
    let client = connect(&a).await;
    for s in ["audit", "auth", "billing", "org"] {
        client
            .batch_execute(&format!("DROP SCHEMA IF EXISTS {s} CASCADE"))
            .await
            .expect("wipe");
    }
    client
        .batch_execute("DROP TABLE IF EXISTS public._jwc_migrations")
        .await
        .expect("wipe");

    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("migrations");
    std::fs::create_dir_all(&dir).expect("mkdir");

    let sample = repo_root().join("docs/spec/v1/sample");
    let ws = Workspace::load(&sample).expect("sample");
    let m = model::build(&ws).model;
    let plan = migrate::plan(&snapshot::Snapshot::default(), &m, 1, "sample");
    let mut prev = snapshot::Snapshot::default();
    for f in &plan.files {
        std::fs::write(dir.join(format!("{}.up.sql", f.stem)), &f.up).expect("write");
        if let Some(s) = &f.snapshot {
            prev = snapshot::Snapshot::from_json(s).expect("re-read");
        }
    }
    apply::up(&client, &dir, None).await.expect("sample up");

    // `number varchar(20) unique` on `billing.Invoices`, projected by
    // `InvoiceDetail`.
    let work = tempfile::tempdir().expect("tempdir");
    for f in std::fs::read_dir(&sample).expect("read sample") {
        copy_tree(&f.expect("entry").path(), work.path());
    }
    let target = work.path().join("src/db/billing.jwc");
    let text = std::fs::read_to_string(&target).expect("billing.jwc");
    let widened = text.replace(
        "number          varchar(20) unique",
        "number          varchar(40) unique",
    );
    assert_ne!(text, widened, "the column this test is about moved");
    std::fs::write(&target, widened).expect("write");

    let ws = Workspace::load(work.path()).expect("widened");
    let m = model::build(&ws).model;
    let plan = migrate::plan(&prev, &m, snapshot::next_ordinal(&dir), "widen_number");
    assert!(!plan.has_errors(), "the widening was refused");
    for f in &plan.files {
        std::fs::write(dir.join(format!("{}.up.sql", f.stem)), &f.up).expect("write");
    }
    apply::up(&client, &dir, None).await.expect("widen up");

    let row = client
        .query_one(
            "SELECT character_maximum_length FROM information_schema.columns
              WHERE table_schema = 'billing' AND table_name = 'invoices'
                AND column_name = 'number'",
            &[],
        )
        .await
        .expect("query");
    assert_eq!(row.get::<_, i32>(0), 40);

    let problems = apply::verify(&client, &snapshot::of(&m))
        .await
        .expect("verify");
    assert!(problems.is_empty(), "{problems:?}");
}

fn copy_tree(from: &Path, into: &Path) {
    let target = into.join(from.file_name().expect("name"));
    if from.is_dir() {
        std::fs::create_dir_all(&target).expect("mkdir");
        for e in std::fs::read_dir(from).expect("read_dir") {
            copy_tree(&e.expect("entry").path(), &target);
        }
    } else {
        std::fs::copy(from, &target).expect("copy");
    }
}
