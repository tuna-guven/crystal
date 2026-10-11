//! Crystal: an encrypted Markdown vault in a TUI.
//!
//! See docs/DESIGN.md for the architecture and threat model.

#![forbid(unsafe_code)]

mod backup;
mod crypto;
mod harden;
mod md;
mod model;
mod sys;
mod term;
mod ui;
mod vault;

fn main() {}
