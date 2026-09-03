//! UI-framework-agnostic application state and the transfer manager.
//!
//! [`App`] is the single handle a frontend holds. Its methods are synchronous
//! and thread-safe (callable from a GUI thread with no tokio runtime); all the
//! async work — snapshotting folders, running swarm downloads, materializing
//! results — happens on a background task [`App::new`] spawns. The frontend
//! reads [`App::snapshot`] (a cloneable [`AppState`]) and re-renders on change,
//! or listens on [`App::events`].
//!
//! Nothing here depends on `gpui`; the `gui` crate is a thin renderer on top.

mod manager;
mod settings;
mod state;

pub use gaggle_core::{Hash, Invite};
pub use manager::{App, AppEvent, AcceleratorRequest, SubscribeRequest};
pub use net::ShareLink;
pub use net::{CacheStats, Multiaddr, PeerId, Scope};
pub use settings::{RemoteAccelerator, Settings, Theme};
pub use state::{
    AcceleratorRole, AcceleratorState, AccelShareRow, AppState, BenchmarkResult, MintedInvite,
    RemoteAccelState, SourceStats, SwarmStatus, TransferId, TransferKind, TransferRow,
    TransferStatus,
};
