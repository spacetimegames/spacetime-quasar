//! Operating-system abstractions and per-OS integration.
//!
//! Purpose: OS abstractions (file system, threads, async, input devices) and per-OS integration (window decorations, VSync, HiDPI, desktop entry). Native-only code is `cfg`-gated here, and the crate provides stubs on `wasm32`.
//!
//! Boundary: this crate is a stub; it has no public API yet.
//!
//! Non-goals: Contains no game logic, and no other crate should carry platform `cfg` gates of its own.
