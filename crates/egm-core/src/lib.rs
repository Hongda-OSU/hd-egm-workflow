//! Core of HD EGM Workflow: Notion pulling, Markdown conversion and round
//! storage. Shared by the CLI now and the Tauri app later.

pub mod agents;
pub mod markdown;
pub mod model;
pub mod notion;
pub mod prd;
pub mod review;
pub mod round;
pub mod settings;
pub mod store;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;
