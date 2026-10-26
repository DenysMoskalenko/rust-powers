//! The one integration-test binary. Every file directly in `tests/` is a binary of its
//! own that links the whole dependency graph again, so areas are modules here instead.

mod common;
mod health;
mod migrations;
mod panic;
mod users;
