//! The commands: each reads its inputs, calls the pure logic, and writes its outputs. This is
//! the only layer that touches files.

pub mod automerge;
pub mod cooldown;
pub mod coverage;
pub mod sync;
pub mod tokens;
pub mod wasm_budget;
