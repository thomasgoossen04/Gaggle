//! Shared application state and the transfer manager. UI-framework-agnostic so it
//! stays testable headless; the GUI's entities wrap the types defined here.

pub use gaggle_core::manifest;

/// High-level snapshot the GUI subscribes to. Fleshed out in milestone 8.
#[derive(Debug, Default)]
pub struct AppState {
    pub shares: Vec<manifest::Manifest>,
}

impl AppState {
    pub fn new() -> Self {
        Self::default()
    }
}
