//! v0.4.0 hash builtins via the interpreter. Known-vector values mirror the
//! unit tests in `src/hash.rs`; this exercises the full parse→run path and the
//! argon2 password round-trip.

use jwc::parser::{parse_program, validate_program};
use jwc::runner::run_main;

async fn run(src: &str) -> String {
    let program = parse_program(src).unwrap_or_else(|e| panic!("parse failed: {e}"));
    validate_program(&program).unwrap_or_else(|e| panic!("validate failed: {e}"));
    run_main(&program)
        .await
        .unwrap_or_else(|e| panic!("run failed: {e}"))
        .output
}

#[tokio::test]
async fn digest_known_vectors() {
    let out = run(r#"
        function main() {
            print(sha256("hello"));
            print(sha1("abc"));
            print(md5("abc"));
            print(hmac_sha256("Jefe", "what do ya want for nothing?"));
        }
    "#)
    .await;
    assert_eq!(
        out,
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824\n\
         a9993e364706816aba3e25717850c26c9cd0d89d\n\
         900150983cd24fb0d6963f7d28e17f72\n\
         5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843\n"
    );
}

#[tokio::test]
async fn password_hash_round_trip() {
    let out = run(r#"
        function main() {
            let h = hash_password("hunter2");
            print(verify_password("hunter2", h));
            print(verify_password("wrong", h));
        }
    "#)
    .await;
    assert_eq!(out, "true\nfalse\n");
}
