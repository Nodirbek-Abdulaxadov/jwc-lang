// Parser fuzz target.
//
// Goal: prove `jwc::parser::parse_program` cannot panic on arbitrary
// input. Returning `Err` is a fine outcome — the CLI surfaces it as a
// diagnostic. A panic is a bug (e.g. arithmetic overflow, slice index,
// unchecked unwrap on a token field) and is what libFuzzer will report.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Same UTF-8 gate as the lexer target. The CLI never feeds non-UTF-8
    // into the parser, so fuzzing that path would produce findings the
    // production code path can't actually hit.
    if let Ok(src) = std::str::from_utf8(data) {
        let _ = jwc::parser::parse_program(src);
    }
});
