//! remote-sync-explorer: a dual-pane terminal file manager built on rclone.

// First, so that `tr!` is in scope in the modules below.
#[macro_use]
pub mod i18n;

pub mod app;
pub mod config;
pub mod endpoint;
pub mod engine;
pub mod event;
pub mod exec;
#[cfg(fuzzing)]
pub mod fuzz;
pub mod hitmap;
pub mod jobs;
pub mod keymap;
pub mod location;
pub mod rclone;
pub mod rsync;
pub mod state;
pub mod terminal;
#[cfg(test)]
mod testutil;
pub mod ui;
