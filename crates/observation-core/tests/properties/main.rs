//! Property tests: invariants of the core's code, checked on generated
//! input.
//!
//! The suites are deterministic: every run uses the same seed (set
//! `PROPTEST_RNG_SEED` to explore others, and `PROPTEST_CASES` to change how
//! many cases a property tries), and failures are not written to regression
//! files. A failure prints the minimal input. Turn it into an ordinary unit
//! test next to the fix, so it is kept as code rather than as a seed file.
//! Case counts are kept small enough that the whole suite takes a few seconds
//! in a debug build.

mod coverage;
mod formats;
mod geo;
mod oracles;
mod strategies;
mod terrain;
mod time;

use proptest::test_runner::{Config, RngSeed};

/// The seed every run uses unless `PROPTEST_RNG_SEED` is set.
const SEED: u64 = 0x0b5e_7a71_0000_0001;

/// `cases` cases, a fixed seed and no failure persistence.
pub fn config(cases: u32) -> Config {
    let mut c = Config::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        c.cases = cases;
    }
    c.failure_persistence = None;
    if std::env::var_os("PROPTEST_RNG_SEED").is_none() {
        c.rng_seed = RngSeed::Fixed(SEED);
    }
    c
}
