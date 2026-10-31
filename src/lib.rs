#[macro_use]
pub mod logger;

pub mod command;
pub mod config;
pub mod errors;
pub mod ha;
pub mod profile;
pub mod remote;
pub mod repo;
pub mod ssh;
pub mod state;
pub mod utils;
pub mod web;

/// Set at build time with APP_VERSION (CI/Docker), falls back to the crate version.
pub const APP_VERSION: &str = match option_env!("APP_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};
