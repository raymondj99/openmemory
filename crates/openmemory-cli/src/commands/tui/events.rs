//! Normalised event stream for the TUI.
//!
//! crossterm exposes terminal events as a flat `Event` enum. The TUI's
//! event loop wants a richer vocabulary: tick (for live counters),
//! key, resize, and background-data delivery. [`AppEvent`] is that
//! vocabulary; [`EventSource`] is the producer.
//!
//! The producer runs entirely on the main thread: each iteration of
//! the event loop polls crossterm with a short timeout, and we
//! synthesise a [`AppEvent::Tick`] whenever the wall clock advances
//! past the next tick deadline. We deliberately avoid a separate
//! tick thread — for a single-threaded UI loop a poll-driven tick is
//! simpler, has no shared state, and (since `poll` returns within
//! `poll_timeout`) keeps the worst-case redraw latency well under
//! 250 ms.
//!
//! Background work (recall queries, future graph spreads) lives on a
//! short-lived worker thread that sends [`AppEvent::DataRefreshed`]
//! variants back to the loop over an `mpsc` channel. The receiver is
//! drained in [`App::poll_event`] alongside the crossterm poll.

use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyEvent};

/// Normalised input event for the App. Kept deliberately small —
/// every screen handles only the variants it cares about and ignores
/// the rest. `Resize` carries the new width/height for screens that
/// will want to reflow on resize (Graph layout in PR4); v1 just
/// triggers a redraw on the next tick.
#[derive(Debug, Clone)]
pub enum AppEvent {
    Key(KeyEvent),
    Resize(#[allow(dead_code)] u16, #[allow(dead_code)] u16),
    /// Wall-clock tick (~every `TICK_INTERVAL`). Drives the Stats
    /// panel's auto-refresh.
    Tick,
}

/// How long [`EventSource::poll`] blocks on crossterm before
/// returning `Ok(None)`. Short enough that the tick deadline check
/// runs at sub-second cadence; long enough that an idle TUI doesn't
/// burn CPU.
pub const POLL_TIMEOUT: Duration = Duration::from_millis(200);

/// Cadence at which the loop emits [`AppEvent::Tick`]. The Stats panel
/// re-reads `MemoryStore::status` every tick.
pub const TICK_INTERVAL: Duration = Duration::from_secs(2);

/// Owns the next-tick deadline so the loop can ask "is it time to
/// tick yet?" without storing the bookkeeping in [`crate::commands::tui::app::App`].
#[derive(Debug)]
pub struct EventSource {
    next_tick: Instant,
}

impl EventSource {
    pub fn new() -> Self {
        Self {
            next_tick: Instant::now() + TICK_INTERVAL,
        }
    }

    /// Poll crossterm for an event with a bounded wait. Returns
    /// `Ok(None)` if the poll window elapses with no input — the
    /// caller then checks [`Self::take_due_tick`] and redraws.
    pub fn poll(&self) -> anyhow::Result<Option<AppEvent>> {
        if !event::poll(POLL_TIMEOUT)? {
            return Ok(None);
        }
        match event::read()? {
            Event::Key(k) => Ok(Some(AppEvent::Key(k))),
            Event::Resize(w, h) => Ok(Some(AppEvent::Resize(w, h))),
            // Mouse, paste, focus events are ignored in v1.
            _ => Ok(None),
        }
    }

    /// If the next-tick deadline has passed, advance it and return
    /// `Some(Tick)`. Idempotent across rapid calls (we always advance
    /// by exactly one [`TICK_INTERVAL`] so a slow frame doesn't queue
    /// up multiple ticks).
    pub fn take_due_tick(&mut self) -> Option<AppEvent> {
        let now = Instant::now();
        if now >= self.next_tick {
            self.next_tick = now + TICK_INTERVAL;
            Some(AppEvent::Tick)
        } else {
            None
        }
    }
}

impl Default for EventSource {
    fn default() -> Self {
        Self::new()
    }
}
