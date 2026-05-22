//! Integration tests for the `import` / `namespace` / `public` system.
//!
//! These cover the language-level surface only — no registry, no resolver,
//! no lockfile. The runner is exercised end-to-end against an in-memory
//! merged Program assembled from two synthetic "files".

use jwc::ast::{Program, Visibility};
use jwc::parser::{parse_program, validate_program};

/// Combine two parsed file Programs (per-file `parse_program` output) into
/// the merged Program shape the runner sees after `project::load_project_from_root`.
fn merge_two(a: Program, b: Program) -> Program {
    let mut combined = a;
    combined.dbcontexts.extend(b.dbcontexts);
    combined.models.extend(b.models);
    combined.routes.extend(b.routes);
    combined.functions.extend(b.functions);
    combined.middlewares.extend(b.middlewares);
    combined.imports.extend(b.imports);
    combined.mounts.extend(b.mounts);
    if let Some(eh) = b.error_handler {
        assert!(combined.error_handler.is_none(), "two errorHandlers");
        combined.error_handler = Some(eh);
    }
    combined
}

#[test]
fn public_function_in_namespaced_file_is_tagged_correctly() {
    let lib = parse_program(
        r#"
        namespace math;
        public function add(a: int, b: int): int {
            return a + b;
        }
        "#,
    )
    .unwrap();
    assert_eq!(lib.functions.len(), 1);
    let f = &lib.functions[0];
    assert_eq!(f.namespace, vec!["math".to_string()]);
    assert!(matches!(f.visibility, Visibility::Public));
}

#[test]
fn function_without_modifier_defaults_to_private() {
    let prog = parse_program(
        r#"
        namespace math;
        function helper() { return 1; }
        "#,
    )
    .unwrap();
    let f = &prog.functions[0];
    assert!(matches!(f.visibility, Visibility::Private));
}

#[test]
fn root_namespace_when_no_namespace_decl() {
    let prog = parse_program(
        r#"
        function main() { print("hi"); }
        "#,
    )
    .unwrap();
    assert!(prog.functions[0].namespace.is_empty());
}

#[test]
fn namespace_decl_must_come_before_decls() {
    let err = parse_program(
        r#"
        function main() {}
        namespace late;
        "#,
    )
    .unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("namespace"),
        "expected namespace ordering error, got: {err}"
    );
}

#[test]
fn duplicate_namespace_declarations_in_one_file_rejected() {
    let err = parse_program(
        r#"
        namespace a;
        namespace b;
        "#,
    )
    .unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("namespace"),
        "expected duplicate-namespace error, got: {err}"
    );
}

#[test]
fn import_collects_imports_scoped_to_file_namespace() {
    let prog = parse_program(
        r#"
        namespace consumer;
        import math;
        import utils.strings;
        function go() {}
        "#,
    )
    .unwrap();
    assert_eq!(prog.imports.len(), 2);
    assert_eq!(prog.imports[0].path, vec!["math"]);
    assert_eq!(prog.imports[0].in_namespace, vec!["consumer"]);
    assert_eq!(prog.imports[1].path, vec!["utils".to_string(), "strings".to_string()]);
}

#[test]
fn mount_with_and_without_prefix_parses() {
    let prog = parse_program(
        r#"
        mount lib_http;
        mount auth at "/api/v1/auth";
        "#,
    )
    .unwrap();
    assert_eq!(prog.mounts.len(), 2);
    assert_eq!(prog.mounts[0].target, vec!["lib_http".to_string()]);
    assert!(prog.mounts[0].prefix.is_none());
    assert!(prog.mounts[0].middlewares.is_empty());

    assert_eq!(prog.mounts[1].target, vec!["auth".to_string()]);
    assert_eq!(prog.mounts[1].prefix.as_deref(), Some("/api/v1/auth"));
}

#[test]
fn group_applies_prefix_and_middleware_to_routes() {
    let prog = parse_program(
        r#"
        middleware Cors { return null; }
        middleware Auth { return null; }

        group "/api" use Cors, Auth {
            route GET "/ping" {
                return json({ ok: true });
            }
        }
        "#,
    )
    .unwrap();
    assert_eq!(prog.routes.len(), 1);
    assert_eq!(prog.routes[0].path, "/api/ping");
    assert_eq!(
        prog.routes[0].middlewares,
        vec!["Cors".to_string(), "Auth".to_string()]
    );
}

#[test]
fn group_applies_to_mount_inside() {
    let prog = parse_program(
        r#"
        middleware Cors { return null; }

        group "/api" use Cors {
            mount greet at "/greet";
        }
        "#,
    )
    .unwrap();
    assert_eq!(prog.mounts.len(), 1);
    assert_eq!(prog.mounts[0].prefix.as_deref(), Some("/api/greet"));
    assert_eq!(prog.mounts[0].middlewares, vec!["Cors".to_string()]);
}

#[test]
fn nested_groups_compose() {
    let prog = parse_program(
        r#"
        middleware Cors { return null; }
        middleware ApiKey { return null; }

        group "/api" use Cors {
            group "/v1" use ApiKey {
                route GET "/users" { return json({ ok: true }); }
            }
        }
        "#,
    )
    .unwrap();
    assert_eq!(prog.routes.len(), 1);
    assert_eq!(prog.routes[0].path, "/api/v1/users");
    assert_eq!(
        prog.routes[0].middlewares,
        vec!["Cors".to_string(), "ApiKey".to_string()]
    );
}

#[test]
fn group_without_prefix_or_middleware_is_rejected() {
    let err = parse_program(
        r#"
        group {
            route GET "/x" { return json({}); }
        }
        "#,
    )
    .unwrap_err();
    assert!(err.to_string().to_lowercase().contains("group"));
}

#[test]
fn cross_namespace_validate_passes_with_distinct_fqns() {
    // Two files, same simple model name in different namespaces — should
    // be accepted post-merge.
    let a = parse_program(
        r#"
        namespace lib_a;
        public class User { id int; }
        "#,
    )
    .unwrap();
    let b = parse_program(
        r#"
        namespace lib_b;
        public class User { id int; }
        "#,
    )
    .unwrap();
    let merged = merge_two(a, b);
    validate_program(&merged).expect("distinct namespaces should not collide");
}

#[test]
fn duplicate_within_same_namespace_rejected() {
    let a = parse_program(
        r#"
        namespace shared;
        public function go() {}
        "#,
    )
    .unwrap();
    let b = parse_program(
        r#"
        namespace shared;
        function go() {}
        "#,
    )
    .unwrap();
    let merged = merge_two(a, b);
    let err = validate_program(&merged).unwrap_err();
    assert!(err.to_string().to_lowercase().contains("duplicate"));
}

#[test]
fn explicit_private_modifier_is_accepted() {
    let prog = parse_program(
        r#"
        namespace math;
        private function helper(): int { return 1; }
        public function go(): int { return helper(); }
        "#,
    )
    .unwrap();
    assert!(matches!(prog.functions[0].visibility, Visibility::Private));
    assert!(matches!(prog.functions[1].visibility, Visibility::Public));
}

#[test]
fn duplicate_visibility_modifier_rejected() {
    let err = parse_program(
        r#"
        public public function go() {}
        "#,
    )
    .unwrap_err();
    assert!(err.to_string().to_lowercase().contains("visibility"));
}

#[test]
fn route_middleware_use_keyword_still_works() {
    // `use` survives — it's the middleware-attach keyword on routes.
    // Package import uses `import`, not `use`.
    let prog = parse_program(
        r#"
        middleware AuthMw { return 1; }
        route GET "/x" use AuthMw {
            return json("ok");
        }
        "#,
    )
    .unwrap();
    assert_eq!(prog.middlewares.len(), 1);
    assert_eq!(prog.routes.len(), 1);
    assert_eq!(prog.routes[0].middlewares, vec!["AuthMw".to_string()]);
}

#[test]
fn root_namespace_call_resolves_without_qualification() {
    // A pure root-namespace program (the legacy form) must still work.
    let prog = parse_program(
        r#"
        function helper(x: int): int { return x + 1; }
        function main() {
            let y = helper(41);
            print(y);
        }
        "#,
    )
    .unwrap();
    validate_program(&prog).unwrap();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = rt.block_on(jwc::runner::run_main(&prog)).unwrap();
    assert!(result.output.contains("42"));
}
