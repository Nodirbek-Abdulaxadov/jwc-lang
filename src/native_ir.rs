//! JWC IR — placeholder for the LLVM-backed native pipeline.
//!
//! Status: **skeleton only**. The existing native backend
//! (`src/native_build.rs`) generates Rust source and hands it to cargo
//! for the final lower-to-machine-code step. That path is in production
//! today and stays the default.
//!
//! This module marks the alternative Phase 4.1 / Sprint 13 lane:
//! lowering JWC AST → a small typed three-address IR → LLVM IR via
//! `inkwell`, skipping cargo for faster cold-start binaries and
//! eventually shorter compile times on small programs.
//!
//! ## Why a separate IR instead of going AST → LLVM directly?
//!
//! - Constant folding, dead-code elimination, and route dispatch
//!   inlining are cheap to express on a linear three-address form and
//!   hard to express on the recursive AST.
//! - The same IR can target multiple backends (LLVM today, cranelift /
//!   WASM later) without re-walking the AST.
//! - A typed IR makes the planned `Value::Bytes` variant (Sprint 2 v2)
//!   and the sema-pass-based field check (Sprint 6) easier to land
//!   without disturbing the interpreter.
//!
//! ## Migration plan (tracked under ROADMAP Sprint 13)
//!
//! 1. Define `IrModule`, `IrFunction`, `IrBlock`, `IrInst` (this file).
//! 2. AST → IR lowering (`lower_program`), starting with the subset
//!    `native_build::reject_unsupported` already accepts.
//! 3. IR → LLVM via `inkwell`, behind a `cargo feature = "llvm"` gate
//!    so the default `jwc` binary stays inkwell-free.
//! 4. `jwc build --native --llvm` flag wired into `cmd::build` once
//!    the AOT path can compile the test suite end-to-end.
//!
//! The skeleton types below intentionally start minimal — adding a
//! variant should be a Definition-of-Done checkpoint, not a yak shave.

use crate::ast::Program;

/// Top-level IR container — one `IrModule` per JWC program.
#[derive(Debug, Default)]
pub struct IrModule {
    pub name: String,
    pub functions: Vec<IrFunction>,
}

/// A lowered function: header + linear block list.
#[derive(Debug)]
pub struct IrFunction {
    pub name: String,
    pub blocks: Vec<IrBlock>,
}

/// Basic block of three-address instructions terminated by an
/// `IrInst::Branch` or `IrInst::Return`.
#[derive(Debug)]
pub struct IrBlock {
    pub label: String,
    pub insts: Vec<IrInst>,
}

/// Three-address instruction set. Intentionally small — grows as
/// lowering work for each AST construct lands.
#[derive(Debug)]
pub enum IrInst {
    /// `dst = const value`
    ConstStr { dst: String, value: String },
    /// `dst = const int`
    ConstInt { dst: String, value: i64 },
    /// `<no-op terminator placeholder>` — Sprint 13 lowering replaces
    /// this with the real Return / Branch variants.
    Nop,
}

/// Stub lowering pass — produces an empty module today so downstream
/// scaffolding (LLVM emitter, CLI flag wiring) has something concrete
/// to build against. Returns `Err` so callers can't accidentally ship
/// a half-built IR pipeline thinking it lowered the program.
pub fn lower_program(program: &Program) -> Result<IrModule, &'static str> {
    let _ = program;
    Err("native_ir lowering is a Sprint 13 skeleton — not yet wired (see src/native_ir.rs doc comment)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{parse_program, validate_program};

    #[test]
    fn lower_program_returns_skeleton_error() {
        let src = r#"function main() { print("hi"); }"#;
        let program = parse_program(src).expect("parse");
        validate_program(&program).expect("validate");
        let result = lower_program(&program);
        assert!(
            result.is_err(),
            "skeleton lower_program must error until Sprint 13 lands"
        );
    }

    #[test]
    fn ir_module_default_is_empty() {
        let m = IrModule::default();
        assert!(m.name.is_empty());
        assert!(m.functions.is_empty());
    }
}
