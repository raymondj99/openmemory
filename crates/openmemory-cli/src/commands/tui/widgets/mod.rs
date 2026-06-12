//! Reusable widgets used across screens.
//!
//! These are pure ratatui composites: stateless renderers that take
//! the [`crate::commands::tui::theme::Theme`] and a frame area, and
//! draw chrome or overlays. Screen-specific widgets live next to
//! their screen.

pub mod chrome;
pub mod help_overlay;
