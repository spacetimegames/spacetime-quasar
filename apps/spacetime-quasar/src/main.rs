//! The Spacetime Quasar game binary.
//!
//! Purpose: a thin binary that glues the workspace crates together. For now
//! it opens a window, clears it to a constant colour through `st-render`
//! (needed for the window to map on Wayland, see ADR 0005), and exits cleanly
//! on close or Escape.
//!
//! Non-goals: no game logic lives here.
//!
//! Debug flag: `--exit-after-ms <n>` exits with status 0 after `n`
//! milliseconds, so the window can be smoke-tested without a human.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "exempt from PD-06: ADR 0008"
)]

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::future::Future;
    use std::sync::Arc;
    use std::task::{Context, Poll, Waker};
    use std::time::{Duration, Instant};

    use st_render::{FrameStatus, WindowSurface};
    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
    use winit::keyboard::{Key, NamedKey};
    use winit::window::{Window, WindowId};

    /// Delay before retrying a frame that could not be drawn.
    const RETRY_DELAY: Duration = Duration::from_millis(100);
    /// Consecutive `RedrawNow` results allowed before backing off.
    const MAX_REDRAW_NOW: u32 = 3;
    /// Longest time `block_on` waits for a future.
    const BLOCK_ON_TIMEOUT: Duration = Duration::from_secs(5);

    /// Drives a future to completion on the current thread.
    ///
    /// wgpu's native futures complete on the first poll. On `Pending` this
    /// yields and polls again (a no-op waker never wakes a parked thread) and
    /// gives up after [`BLOCK_ON_TIMEOUT`].
    fn block_on<F: Future>(future: F) -> Result<F::Output, String> {
        let mut future = std::pin::pin!(future);
        let mut cx = Context::from_waker(Waker::noop());
        let start = Instant::now();
        loop {
            if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
                return Ok(value);
            }
            if start.elapsed() >= BLOCK_ON_TIMEOUT {
                return Err("timed out waiting for the GPU".to_owned());
            }
            std::thread::yield_now();
        }
    }

    /// Window state driven by the winit event loop.
    #[derive(Default)]
    struct App {
        /// The main window, created on the first `resumed` event.
        window: Option<Arc<Window>>,
        /// The GPU surface of the window.
        surface: Option<WindowSurface>,
        /// Optional debug deadline after which the app exits.
        deadline: Option<Instant>,
        /// When to retry a frame that could not be drawn.
        retry: Option<Instant>,
        /// Consecutive `RedrawNow` results.
        redraw_now: u32,
        /// Consecutive `SurfaceLost` results.
        lost: u32,
        /// Set when the app is exiting because of an error.
        failed: bool,
    }

    impl App {
        /// Prints a fatal error and stops the event loop with failure.
        fn fail(&mut self, event_loop: &ActiveEventLoop, message: &str) {
            eprintln!("error: {message}");
            self.failed = true;
            event_loop.exit();
        }

        fn redraw(&mut self, event_loop: &ActiveEventLoop) {
            let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else {
                return;
            };
            let size = window.inner_size();
            let status = surface.render(size.width, size.height, || window.pre_present_notify());
            let mut status = status;
            if status == FrameStatus::RedrawNow {
                if self.redraw_now >= MAX_REDRAW_NOW {
                    self.redraw_now = 0;
                    status = FrameStatus::RedrawLater;
                } else {
                    self.redraw_now += 1;
                }
            }
            if status == FrameStatus::Presented {
                self.redraw_now = 0;
                self.lost = 0;
            }
            match status {
                FrameStatus::Presented | FrameStatus::Idle => {}
                FrameStatus::RedrawNow => window.request_redraw(),
                FrameStatus::RedrawLater => self.retry = Some(Instant::now() + RETRY_DELAY),
                FrameStatus::SurfaceLost => {
                    self.lost += 1;
                    if self.lost >= 2 {
                        self.fail(
                            event_loop,
                            "the GPU surface was lost and could not be recovered",
                        );
                    } else if let Err(err) = surface.recreate_surface(window.clone()) {
                        self.fail(event_loop, &err.to_string());
                    } else {
                        window.request_redraw();
                    }
                }
            }
        }
    }

    impl ApplicationHandler for App {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let attrs = Window::default_attributes().with_title("Spacetime Quasar");
            let window = match event_loop.create_window(attrs) {
                Ok(window) => Arc::new(window),
                Err(err) => return self.fail(event_loop, &format!("cannot create window: {err}")),
            };
            let size = window.inner_size();
            let display = Box::new(event_loop.owned_display_handle());
            let created = block_on(WindowSurface::new(
                display,
                window.clone(),
                size.width,
                size.height,
            ));
            match created {
                Ok(Ok(surface)) => {
                    let info = surface.adapter_info();
                    println!("adapter: {} ({:?})", info.name, info.backend);
                    self.surface = Some(surface);
                }
                Ok(Err(err)) => return self.fail(event_loop, &err.to_string()),
                Err(err) => return self.fail(event_loop, &err),
            }
            window.request_redraw();
            self.window = Some(window);
        }

        fn window_event(
            &mut self,
            event_loop: &ActiveEventLoop,
            _id: WindowId,
            event: WindowEvent,
        ) {
            match event {
                WindowEvent::CloseRequested => event_loop.exit(),
                WindowEvent::KeyboardInput { event, .. }
                    if event.state.is_pressed()
                        && matches!(event.logical_key, Key::Named(NamedKey::Escape)) =>
                {
                    event_loop.exit();
                }
                WindowEvent::Resized(_) | WindowEvent::Occluded(false) => {
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
                WindowEvent::RedrawRequested => self.redraw(event_loop),
                _ => {}
            }
        }

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            let now = Instant::now();
            if self.deadline.is_some_and(|d| now >= d) {
                event_loop.exit();
                return;
            }
            if self.retry.is_some_and(|r| now >= r) {
                self.retry = None;
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            match self.deadline.into_iter().chain(self.retry).min() {
                Some(wake) => event_loop.set_control_flow(ControlFlow::WaitUntil(wake)),
                None => event_loop.set_control_flow(ControlFlow::Wait),
            }
        }
    }

    /// Parses `--exit-after-ms <n>` from the command line.
    fn exit_after() -> Option<Duration> {
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            if arg == "--exit-after-ms" {
                return args.next()?.parse().ok().map(Duration::from_millis);
            }
        }
        None
    }

    /// Runs the event loop until the window is closed; exits with status 1 on
    /// a setup error.
    pub fn run() {
        let event_loop = match EventLoop::new() {
            Ok(event_loop) => event_loop,
            Err(err) => {
                eprintln!("error: cannot create event loop: {err}");
                std::process::exit(1);
            }
        };
        event_loop.set_control_flow(ControlFlow::Wait);
        let mut app = App {
            deadline: exit_after().map(|d| Instant::now() + d),
            ..App::default()
        };
        if let Err(err) = event_loop.run_app(&mut app) {
            eprintln!("error: event loop: {err}");
            std::process::exit(1);
        }
        if app.failed {
            std::process::exit(1);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    native::run();
}

/// The browser build is post-MVP; this stub keeps the target compiling.
#[cfg(target_arch = "wasm32")]
fn main() {}
