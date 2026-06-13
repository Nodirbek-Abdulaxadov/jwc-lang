// Lexer fuzz target.
//
// Goal: prove the hand-written tokeniser in `src/lexer.rs` cannot panic on
// arbitrary input. The lexer does NOT expose a `pub fn tokenize` — its
// public surface is `Lexer::new(&str)` + `next_token() -> Result<Token>`.
// We drive the lexer to EOF (or to its first lex error), discarding both
// outcomes; only an unwrap/index/arithmetic panic counts as a finding.

#![no_main]

use libfuzzer_sys::fuzz_target;

use jwc::lexer::{Lexer, TokenKind};

fuzz_target!(|data: &[u8]| {
    // libFuzzer hands us raw bytes. The lexer operates on `&str`, so we
    // need valid UTF-8. Rejecting bad UTF-8 here is correct: the CLI
    // reads source via `std::fs::read_to_string`, which would refuse
    // non-UTF-8 long before the lexer sees it.
    let Ok(src) = std::str::from_utf8(data) else {
        return;
    };

    let mut lexer = Lexer::new(src);
    // Bound the loop. A pathological input could in theory produce
    // zero-width tokens forever; cap iterations at input length + 1 so
    // we always terminate. The lexer is well-behaved in practice but
    // this is cheap insurance for the fuzzer.
    let max_iters = src.len().saturating_add(1);
    for _ in 0..max_iters {
        match lexer.next_token() {
            Ok(tok) => {
                if matches!(tok.kind, TokenKind::Eof) {
                    break;
                }
            }
            Err(_) => break,
        }
    }
});
