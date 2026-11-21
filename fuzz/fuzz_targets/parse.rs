//! Untrusted source text through the front end (D-018): oxc's parser,
//! oxc_semantic's early errors and our own early-error pass. The first byte
//! picks the goal (script or module); the rest must be UTF-8, like any
//! source the embedder hands us after decoding. Parsing must never panic,
//! and every rejection must carry at least one error.
#![no_main]

use ferrojs_compiler::{parse, Goal};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let [g, rest @ ..] = data else {
        return;
    };
    let Ok(source) = std::str::from_utf8(rest) else {
        return;
    };
    let goal = if g & 1 == 0 {
        Goal::Script
    } else {
        Goal::Module
    };
    let allocator = ferrojs_compiler::Allocator::default();
    if let Err(errors) = parse(&allocator, source, goal) {
        assert!(!errors.is_empty(), "a rejection must carry an error");
    }
});
