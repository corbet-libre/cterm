//! Bounded parser fuzzer: fixed seed corpus, iteration cap, resource bounds.
//!
//! This is a deterministic Rust test, not a CI job: it runs with
//! `cargo test -p cterm-core` on every host with no external binaries.
//! Mutations derive from a fixed xorshift64 seed, so failures reproduce
//! byte-for-byte. The test asserts the parser never panics, hangs, or grows
//! without bound over the capped run.

use cterm_core::screen::{Screen, ScreenConfig};
use cterm_core::Parser;
use std::time::Instant;

/// Fixed seed inputs covering the wire grammar's main shapes.
const SEEDS: &[&[u8]] = &[
    b"hello world",
    b"\x1b[1;3;4:3;38:2::1:2:3m",
    b"\x1b[?1006h\x1b[?1006$p",
    b"\x1b[?2026$p\x1b[?5522$p",
    b"\x1b[6n\x1b[5n\x1b[c\x1b[>c",
    b"\x1bP$qm\x1b\\\x1bP$q q\x1b\\",
    b"\x1bP+q616d\x1b\\",
    b"\x1b]10;#112233\x1b\\\x1b]10;?\x1b\\",
    b"\x1b]21;foreground=#112233;foreground=?\x1b\\",
    b"\x1b]22;pointer\x1b\\\x1b]22;?pointer\x1b\\",
    b"\x1b]52;c;aGk=\x1b\\",
    b"\x1b]66;w=3;B\x07",
    b"\x1b_Ga=T,f=32,s=1,v=1,i=7,C=1;/wAA/w==\x1b\\",
    b"\x1bPq#1;2;0;0;0#@~~\x1b\\",
    b"e\xcc\x81\xf0\x9f\x91\xa9\xe2\x80\x8d\xf0\x9f\x92\xbb\xf0\x9f\x87\xa8\xf0\x9f\x87\xad",
    b"\x1b[2J\x1b[HABCDE\x1b[2D\x1b[2P\x1b[2@\x1b[2X",
    b"\x1b[?1;1;1;2;1;2;1;1$v\x1b[88;1;1;2;3$x",
    b"\x1b[1+T\x1b[1S\x1b[1T\x1b[2L\x1b[2M",
    b"\x1b]133;A\x1b\\\x1b]133;C\x1b\\\x1b]133;D\x1b\\",
    b"\x1b[>1u\x1b[?u\x1b[=1u",
];

const ITERATIONS: usize = 5000;
const MAX_RESPONSE_VECS: usize = 1024;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_KITTY_QUERY_GROUPS: usize = 128;

struct XorShift64(u64);

impl XorShift64 {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound.max(1) as u64) as usize
    }
}

fn mutate(rng: &mut XorShift64, input: &mut Vec<u8>) {
    match rng.below(5) {
        // Flip a byte.
        0 => {
            if !input.is_empty() {
                let at = rng.below(input.len());
                let byte = (rng.next() & 0xff) as u8;
                input[at] = byte;
            }
        }
        // Truncate.
        1 => {
            if !input.is_empty() {
                let at = rng.below(input.len() + 1);
                input.truncate(at);
            }
        }
        // Splice two seeds.
        2 => {
            let other = SEEDS[rng.below(SEEDS.len())];
            let at = rng.below(input.len() + 1);
            let mut spliced = Vec::with_capacity(input.len() + other.len());
            spliced.extend_from_slice(&input[..at]);
            spliced.extend_from_slice(other);
            spliced.extend_from_slice(&input[at..]);
            // Cap single-input size so one hostile case cannot OOM the test.
            spliced.truncate(4096);
            *input = spliced;
        }
        // Insert a hostile byte (ESC, CSI param, OSC terminator fragment).
        3 => {
            const HOSTILE: &[u8] = b"\x1b;?0123456789:$!\"#%\\";
            let at = rng.below(input.len() + 1);
            input.insert(at.min(input.len()), HOSTILE[rng.below(HOSTILE.len())]);
            input.truncate(4096);
        }
        // Duplicate a slice.
        _ => {
            if !input.is_empty() {
                let from = rng.below(input.len());
                let len = rng.below(input.len() - from + 1).min(64);
                let chunk = input[from..from + len].to_vec();
                let at = rng.below(input.len() + 1);
                let mut grown = Vec::with_capacity(input.len() + chunk.len());
                grown.extend_from_slice(&input[..at]);
                grown.extend_from_slice(&chunk);
                grown.extend_from_slice(&input[at..]);
                grown.truncate(4096);
                *input = grown;
            }
        }
    }
}

#[test]
fn bounded_parser_fuzz_never_hangs_or_grows_without_bound() {
    let started = Instant::now();
    let mut rng = XorShift64(0x0123_4567_89ab_cdef);
    let mut screen = Screen::new(80, 24, ScreenConfig::default());
    let mut parser = Parser::new();

    for iteration in 0..ITERATIONS {
        let seed = SEEDS[iteration % SEEDS.len()];
        let mut input = seed.to_vec();
        // 0-3 deterministic mutations per case.
        for _ in 0..rng.below(4) {
            mutate(&mut rng, &mut input);
        }
        parser.parse(&mut screen, &input);

        // Drain every iteration so queues cannot grow across the run.
        let responses = screen.take_pending_responses();
        assert!(
            responses.len() <= MAX_RESPONSE_VECS,
            "iteration {iteration}: {} response vecs",
            responses.len()
        );
        let bytes: usize = responses.iter().map(Vec::len).sum();
        assert!(
            bytes <= MAX_RESPONSE_BYTES,
            "iteration {iteration}: {bytes} response bytes"
        );
        assert!(
            screen.take_kitty_queries().len() <= MAX_KITTY_QUERY_GROUPS,
            "iteration {iteration}: kitty query groups bounded"
        );
        // Screen geometry is fixed by API (no escape resizes the grid), and
        // scrollback stays within its configured cap.
        assert_eq!((screen.width(), screen.height()), (80, 24));
        assert!(screen.scrollback().len() <= 10_000);

        // Periodic reseed keeps one pathological state from dominating the
        // run while preserving determinism.
        if iteration % 1000 == 999 {
            screen = Screen::new(80, 24, ScreenConfig::default());
            parser = Parser::new();
        }
    }

    let elapsed = started.elapsed();
    assert!(
        elapsed.as_secs() < 30,
        "fuzz run took too long (hang risk): {elapsed:?}"
    );

    // The terminal still answers after the full run.
    parser.parse(&mut screen, b"\x1b[5n");
    assert_eq!(screen.take_pending_responses(), vec![b"\x1b[0n".to_vec()]);
}
