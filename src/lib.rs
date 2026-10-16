//! Core shared by the app (mishapi) and the CLI (mishapi-cli).
//!
//! Layers, from the inside out (each one only uses the inner ones):
//! `domain` → `engine` → `infra` / `import` → `ui`.

pub mod domain;
pub mod engine;
pub mod i18n;
pub mod import;
pub mod infra;
pub mod ui;
