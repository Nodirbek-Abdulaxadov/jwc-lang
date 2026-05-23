//! Parse + validate smoke tests for Sprint 6 (`group by` / `having`).
//!
//! These cover the parser/validator surface only — actual SQL execution
//! requires Postgres and is exercised by `integration_db` when Docker is
//! available.

use jwc::parser::{parse_program, validate_program};

fn validate_source(src: &str) -> Result<(), String> {
    let program = parse_program(src).map_err(|e| format!("parse: {e}"))?;
    validate_program(&program).map_err(|e| format!("validate: {e:#}"))
}

#[test]
fn group_by_single_column_parses_and_validates() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity Sale of AppDb {
            id int pk;
            country varchar(64);
            amount int;
        }

        function totalsByCountry() {
            return select Sale from AppDb.Sale group by Sale.country;
        }

        function main() { totalsByCountry(); }
    "#;
    validate_source(src).expect("group by Sale.country must validate");
}

#[test]
fn group_by_multiple_columns() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity Sale of AppDb {
            id int pk;
            country varchar(64);
            currency varchar(8);
            amount int;
        }

        function totalsByPair() {
            return select Sale from AppDb.Sale
                group by Sale.country, Sale.currency;
        }

        function main() { totalsByPair(); }
    "#;
    validate_source(src).expect("group by two columns must validate");
}

#[test]
fn group_by_unknown_column_is_rejected() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity Sale of AppDb {
            id int pk;
            country varchar(64);
        }

        function bogus() {
            return select Sale from AppDb.Sale group by Sale.nope;
        }

        function main() { bogus(); }
    "#;
    let err = validate_source(src).expect_err("unknown group-by column must be rejected");
    assert!(
        err.contains("GROUP BY") && err.contains("nope"),
        "expected GROUP BY column error, got: {err}"
    );
}

#[test]
fn having_with_group_by_validates() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity Sale of AppDb {
            id int pk;
            country varchar(64);
            amount int;
        }

        function topCountries(min) {
            return select Sale from AppDb.Sale
                group by Sale.country
                having Sale.amount > @min;
        }

        function main() { topCountries(100); }
    "#;
    validate_source(src).expect("having with group by must validate");
}

#[test]
fn having_without_group_by_is_rejected() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity Sale of AppDb {
            id int pk;
            amount int;
        }

        function broken() {
            return select Sale from AppDb.Sale having Sale.amount > 0;
        }

        function main() { broken(); }
    "#;
    let err = validate_source(src).expect_err("HAVING without GROUP BY must be rejected");
    assert!(
        err.contains("having") && err.contains("group by"),
        "expected having-without-group-by error, got: {err}"
    );
}
