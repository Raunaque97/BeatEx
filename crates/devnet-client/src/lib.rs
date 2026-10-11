#![allow(dead_code)]

// Keep the binary and library on the same implementation so the CLI and
// long-lived native Rust clients cannot drift apart. The public API is exposed
// by the implementation in main.rs.
include!("main.rs");
