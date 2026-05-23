//! Integration tests for `jwc openapi` — exercises the pure
//! `build_openapi_value` function against a small hand-crafted program.
//!
//! Covers the contract documented in `src/cmd/openapi.rs`:
//! - top-level `openapi == "3.0.3"`
//! - `paths./users.get` exists
//! - `paths./users.post.requestBody` exists
//! - `paths./users/{id}.get.parameters[0].in == "path"`
//! - `components.schemas.User.properties.id` is emitted from the `entity`

use jwc::cmd::openapi::build_openapi_value;
use jwc::parser::{parse_program, validate_program};

const SAMPLE: &str = r#"
dbcontext AppDb : Postgres;

entity User of AppDb {
    id uuid pk;
    name varchar(120);
    email varchar(200);
}

function listUsers() {
    return ok({ items: 0 });
}

function createUser(name: string, email: string) {
    validate body {
        name:  required, minLength(2);
        email: required;
    }
    let req = body();
    return created(req);
}

function getUserById(id: uuid) {
    return ok({ id: id });
}

route GET  "/users"       -> listUsers;
route POST "/users"       -> createUser;
route GET  "/users/{id}"  -> getUserById;
"#;

fn build() -> serde_json::Value {
    let p = parse_program(SAMPLE).expect("parse");
    validate_program(&p).expect("validate");
    build_openapi_value(&p)
}

#[test]
fn top_level_openapi_version_is_3_0_3() {
    let doc = build();
    assert_eq!(doc["openapi"], "3.0.3");
}

#[test]
fn get_users_path_present() {
    let doc = build();
    assert!(
        doc["paths"]["/users"]["get"].is_object(),
        "GET /users must be present, got: {}",
        doc["paths"]
    );
}

#[test]
fn post_users_has_request_body() {
    let doc = build();
    let req = &doc["paths"]["/users"]["post"]["requestBody"];
    assert!(req.is_object(), "POST /users must have requestBody");
    assert!(
        req["content"]["application/json"]["schema"].is_object(),
        "requestBody must carry an application/json schema"
    );
}

#[test]
fn get_user_by_id_first_param_is_path() {
    let doc = build();
    let params = doc["paths"]["/users/{id}"]["get"]["parameters"]
        .as_array()
        .expect("parameters array");
    assert!(!params.is_empty(), "expected at least one parameter");
    assert_eq!(params[0]["in"], "path");
    assert_eq!(params[0]["name"], "id");
    assert_eq!(params[0]["required"], true);
}

#[test]
fn user_entity_appears_in_components_schemas() {
    let doc = build();
    let user = &doc["components"]["schemas"]["User"];
    assert_eq!(user["type"], "object");
    let props = user["properties"]
        .as_object()
        .expect("User.properties must be an object");
    assert!(props.contains_key("id"), "User.properties.id missing");
    assert!(props.contains_key("name"));
    assert!(props.contains_key("email"));
}
