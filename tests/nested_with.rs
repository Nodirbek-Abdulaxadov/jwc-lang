//! Parse + validate tests for two-level nested eager-load
//! (`select Project with boards.lanes`). SQL-shape coverage lives in the
//! `nav_sql_tests` unit module; runtime execution is exercised against
//! Postgres in `integration_db` when Docker is available.

use jwc::parser::{parse_program, validate_program};

fn validate_source(src: &str) -> Result<(), String> {
    let program = parse_program(src).map_err(|e| format!("parse: {e}"))?;
    validate_program(&program).map_err(|e| format!("validate: {e:#}"))
}

const SCHEMA: &str = r#"
    dbcontext AppDb : Postgres;
    entity Project of AppDb {
        id int pk;
        name varchar(64);
        boards: List<Board> via Board.projectId;
    }
    entity Board of AppDb {
        id int pk;
        projectId int;
        lanes: List<Lane> via Lane.boardId;
    }
    entity Lane of AppDb {
        id int pk;
        boardId int;
        name varchar(64);
    }
"#;

#[test]
fn nested_with_parses_and_validates() {
    let src = format!(
        "{SCHEMA}\n function detail() {{ return select Project with boards.lanes from AppDb.Project first; }}\n function main() {{ detail(); }}"
    );
    validate_source(&src).expect("nested `with boards.lanes` must validate");
}

#[test]
fn flat_with_still_validates() {
    let src = format!(
        "{SCHEMA}\n function list() {{ return select Board with lanes from AppDb.Board; }}\n function main() {{ list(); }}"
    );
    validate_source(&src).expect("flat `with lanes` must still validate");
}

#[test]
fn unknown_head_nav_is_rejected() {
    let src = format!(
        "{SCHEMA}\n function bad() {{ return select Project with nope.lanes from AppDb.Project first; }}\n function main() {{ bad(); }}"
    );
    let err = validate_source(&src).expect_err("unknown head nav must be rejected");
    assert!(
        err.contains("Project") && err.contains("nope"),
        "expected head-nav error, got: {err}"
    );
}

#[test]
fn unknown_nested_child_is_rejected() {
    let src = format!(
        "{SCHEMA}\n function bad() {{ return select Project with boards.ghosts from AppDb.Project first; }}\n function main() {{ bad(); }}"
    );
    let err = validate_source(&src).expect_err("unknown nested child must be rejected");
    assert!(
        err.contains("Board") && err.contains("ghosts"),
        "expected child-nav error naming the target entity, got: {err}"
    );
}
