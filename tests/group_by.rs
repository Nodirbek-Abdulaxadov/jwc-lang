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
fn having_on_a_group_key_validates() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity Sale of AppDb {
            id int pk;
            country varchar(64);
            amount int;
        }

        function topCountries(min) {
            return select Sale { country, total: count(*) } from AppDb.Sale
                group by Sale.country
                having Sale.country != @min;
        }

        function main() { topCountries("XX"); }
    "#;
    validate_source(src).expect("having on a group key must validate");
}

/// `having <plain column>` where the column is neither a group key nor an
/// aggregate used to validate and then fail at the database:
///
/// ```text
/// ERROR: column "sale.amount" must appear in the GROUP BY clause
///        or be used in an aggregate function
/// ```
#[test]
fn having_on_a_non_grouped_column_is_rejected() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity Sale of AppDb {
            id int pk;
            country varchar(64);
            amount int;
        }

        function broken(min) {
            return select Sale { country, total: count(*) } from AppDb.Sale
                group by Sale.country
                having Sale.amount > @min;
        }

        function main() { broken(100); }
    "#;
    let err = validate_source(src).expect_err("non-grouped column in HAVING must be rejected");
    assert!(
        err.contains("E010") && err.contains("amount"),
        "expected the HAVING-scope error, got: {err}"
    );
}

#[test]
fn having_accepts_aggregate_comparisons() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity Sale of AppDb {
            id int pk;
            country varchar(64);
            amount int;
        }

        function busyCountries() {
            return select Sale { country, total: count(*) } from AppDb.Sale
                group by Sale.country
                having count(*) > 2 and sum(Sale.amount) >= 500;
        }

        function main() { busyCountries(); }
    "#;
    validate_source(src).expect("aggregate comparisons in having must validate");
}

/// Postgres rejects a SELECT output alias in `HAVING`, so writing the alias —
/// the obvious thing to do — has to be rewritten to the aggregate it names.
#[test]
fn having_accepts_an_aggregate_alias_from_the_projection() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity Sale of AppDb {
            id int pk;
            country varchar(64);
        }

        function busyCountries() {
            return select Sale { country, total: count(*) } from AppDb.Sale
                group by Sale.country
                having total > 2;
        }

        function main() { busyCountries(); }
    "#;
    validate_source(src).expect("an aggregate alias must be usable in having");
}

#[test]
fn having_rejects_an_unknown_column_inside_an_aggregate() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity Sale of AppDb {
            id int pk;
            country varchar(64);
        }

        function broken() {
            return select Sale { country, total: count(*) } from AppDb.Sale
                group by Sale.country
                having sum(Sale.nope) > 2;
        }

        function main() { broken(); }
    "#;
    let err = validate_source(src).expect_err("unknown aggregate column must be rejected");
    assert!(
        err.contains("nope") && err.contains("sum()"),
        "expected the aggregate-column error, got: {err}"
    );
}

/// The aggregate names aren't reserved words — `where` has no aggregate form,
/// so a column called `count` still parses there.
#[test]
fn a_column_named_like_an_aggregate_still_works_in_where() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity Sale of AppDb {
            id int pk;
            count int;
            min int;
        }

        function f(n) {
            return select Sale from AppDb.Sale where Sale.count > @n and Sale.min < @n;
        }

        function main() { f(1); }
    "#;
    validate_source(src).expect("columns named count/min must still be usable in where");
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
