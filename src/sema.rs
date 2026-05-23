//! Semantic analysis pass.
//!
//! This module is the eventual home for everything that today lives inside
//! `parser::validate_program` — context maps, entity tables, FK graphs,
//! cross-entity navigation resolution, and route↔dbcontext consistency
//! checks. Splitting these out of `parser.rs` is a multi-PR effort; for
//! now `analyse` is a thin forwarding shim so callers can already depend
//! on the `sema::` path while the internals migrate over.

use anyhow::Result;

use crate::ast::Program;

/// Semantic analysis pass — currently a thin wrapper around
/// `parser::validate_program` while we incrementally extract the
/// validate-pass logic out of parser.rs. Future PRs will move
/// validator state (context map, entity tables, FK graph) into a
/// dedicated `SemaContext` here.
pub fn analyse(program: &Program) -> Result<()> {
    crate::parser::validate_program(program)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_program;

    #[test]
    fn analyse_accepts_tiny_program() {
        let src = r#"
            entity User {
                id uuid pk;
                name text;
            }

            function main() {
                print("ok");
            }
        "#;
        let program = parse_program(src).expect("parse");
        analyse(&program).expect("sema analyse should accept a valid program");
    }
}
