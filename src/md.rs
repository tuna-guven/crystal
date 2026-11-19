//! Markdown and plain text for the screen: the sanitizer (`safe`), the
//! renderer over pulldown-cmark events (`render`), and the check of which
//! link targets may be clickable (`link`), and the blocks a link can
//! point into (`blocks`).

pub mod blocks;
pub mod link;
pub mod render;
pub mod safe;
