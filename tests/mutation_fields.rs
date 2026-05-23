//! Sprint 6 follow-up: compile-time field-name check on
//! `insert` / `update` / `delete` mutations.
//!
//! The runtime already errors when an `update v` has no dirty fields; this
//! test suite covers the equivalent check at *compile time* — bogus
//! `v.field = ...` writes against a variable bound to `new Entity()` should
//! be rejected before the runtime ever sees them, and the entity bound to
//! the variable must agree with the table named in the DB statement.

use jwc::parser::{parse_program, validate_program};

fn validate_source(src: &str) -> Result<(), String> {
    let program = parse_program(src).map_err(|e| format!("parse: {e}"))?;
    validate_program(&program).map_err(|e| format!("validate: {e:#}"))
}

#[test]
fn unknown_field_assignment_before_insert_is_rejected() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity Product of AppDb {
            id int pk autoincrement;
            name varchar(120);
            price int;
        }

        function addBogus() {
            let v = new Product();
            v.unknown_field = "x";
            insert v into AppDb.Product;
        }

        function main() { addBogus(); }
    "#;
    let err = validate_source(src).expect_err("unknown field must be rejected");
    assert!(
        err.contains("unknown_field"),
        "error should mention the bogus field name, got: {err}"
    );
    assert!(
        err.contains("Product"),
        "error should mention the bound entity, got: {err}"
    );
}

#[test]
fn real_field_assignment_before_insert_validates() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity Product of AppDb {
            id int pk autoincrement;
            name varchar(120);
            price int;
        }

        function addReal() {
            let v = new Product();
            v.name = "widget";
            v.price = 100;
            insert v into AppDb.Product;
        }

        function main() { addReal(); }
    "#;
    validate_source(src).expect("real field assignments must validate");
}

#[test]
fn rebinding_uses_new_entity_fields() {
    // After `v = new B();`, `v.b_only` is legal because the binding now
    // tracks B's fields, not A's. Inserting into `AppDb.B` should also
    // line up with the new binding.
    let src = r#"
        dbcontext AppDb : Postgres;
        entity A of AppDb {
            id int pk autoincrement;
            a_only varchar(64);
        }
        entity B of AppDb {
            id int pk autoincrement;
            b_only varchar(64);
        }

        function rebind() {
            let v = new A();
            v = new B();
            v.b_only = "ok";
            insert v into AppDb.B;
        }

        function main() { rebind(); }
    "#;
    validate_source(src).expect("rebinding to B must use B's fields, not A's");
}

#[test]
fn rebinding_field_not_on_new_entity_is_rejected() {
    // After `v = new B();`, writing to `v.a_only` (which only exists on A)
    // should fail — the binding now points at B.
    let src = r#"
        dbcontext AppDb : Postgres;
        entity A of AppDb {
            id int pk autoincrement;
            a_only varchar(64);
        }
        entity B of AppDb {
            id int pk autoincrement;
            b_only varchar(64);
        }

        function rebindBogus() {
            let v = new A();
            v = new B();
            v.a_only = "x";
            insert v into AppDb.B;
        }

        function main() { rebindBogus(); }
    "#;
    let err = validate_source(src).expect_err("a_only must be rejected on B");
    assert!(
        err.contains("a_only") && err.contains("'B'"),
        "error should mention the bogus field and entity B, got: {err}"
    );
}

#[test]
fn insert_into_mismatched_table_is_rejected() {
    // `v` is bound to `A`, but the insert targets `B`. This should fail.
    let src = r#"
        dbcontext AppDb : Postgres;
        entity A of AppDb {
            id int pk autoincrement;
            name varchar(64);
        }
        entity B of AppDb {
            id int pk autoincrement;
            name varchar(64);
        }

        function crossInsert() {
            let v = new A();
            v.name = "x";
            insert v into AppDb.B;
        }

        function main() { crossInsert(); }
    "#;
    let err = validate_source(src).expect_err("cross-table insert must be rejected");
    assert!(
        err.contains("'A'") && err.contains("B"),
        "error should mention both A and B, got: {err}"
    );
}
