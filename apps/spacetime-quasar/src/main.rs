//! The Spacetime Quasar game binary.
//!
//! Purpose: a thin binary that glues the workspace crates together. For now
//! it opens an empty window and exits cleanly on close or Escape.
//!
//! Non-goals: no game logic lives here, and nothing is drawn yet.
//!
//! Debug flag: `--exit-after-ms <n>` exits with status 0 after `n`
//! milliseconds, so the window can be smoke-tested without a human.

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::time::{Duration, Instant};

    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
    use winit::keyboard::{Key, NamedKey};
    use winit::window::{Window, WindowId};

    /// Window state driven by the winit event loop.
    #[derive(Default)]
    struct App {
        /// The main window, created on the first `resumed` event.
        window: Option<Window>,
        /// Optional debug deadline after which the app exits.
        deadline: Option<Instant>,
    }

    impl ApplicationHandler for App {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let attrs = Window::default_attributes().with_title("Spacetime Quasar");
            match event_loop.create_window(attrs) {
                Ok(window) => self.window = Some(window),
                Err(err) => {
                    eprintln!("failed to create window: {err}");
                    event_loop.exit();
                }
            }
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
                _ => {}
            }
        }

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            if let Some(deadline) = self.deadline {
                if Instant::now() >= deadline {
                    event_loop.exit();
                } else {
                    event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
                }
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

    /// Runs the event loop until the window is closed.
    pub fn run() {
        let event_loop = match EventLoop::new() {
            Ok(event_loop) => event_loop,
            Err(err) => {
                eprintln!("failed to create event loop: {err}");
                std::process::exit(1);
            }
        };
        event_loop.set_control_flow(ControlFlow::Wait);
        let mut app = App {
            window: None,
            deadline: exit_after().map(|d| Instant::now() + d),
        };
        if let Err(err) = event_loop.run_app(&mut app) {
            eprintln!("event loop error: {err}");
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
