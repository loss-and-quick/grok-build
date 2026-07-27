//! Login-method picker shown on the welcome screen.
//!
//! Opened by `/login` and `/switch-account` when the agent advertises more
//! than one *interactive* auth method. [`state`] owns everything a renderer
//! needs (row order, selection, badges, labels); [`render`] is the ratatui
//! layout and input mapping layered on top.

pub mod render;
pub mod state;

pub use render::render_auth_method_picker;
pub use state::{
    AuthMethodDetail, AuthMethodEntry, AuthMethodPickerOutcome, AuthMethodPickerState,
};
