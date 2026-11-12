//! The library behind `cargo xtask`: everything but the command line.
//!
//! - [`core`]: pure helpers over parsed inputs (versions, times, Markdown tables, GitHub Actions
//!   annotations). Nothing in it reads or writes files or runs a program.

pub mod core;
