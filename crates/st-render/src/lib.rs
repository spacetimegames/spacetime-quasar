//! The wgpu renderer.
//!
//! Planned scope: The wgpu renderer, internally layered into core, pbr, shadow, post and reflection, with an N-view camera. Rendering will be GPU-driven.
//!
//! Boundary: today the public API is [`WindowSurface`], which owns the wgpu
//! instance, surface, device and queue, and presents a constant-colour frame
//! so a window becomes visible (ADR 0005). It does not depend on winit: the
//! window is passed in as a [`wgpu::SurfaceTarget`].
//!
//! Non-goals: No CPU rendering path, no simulation state, and no game-specific types in public signatures.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "exempt from PD-06: ADR 0008"
)]

use std::fmt;

/// The constant clear colour of the Phase 0 window (a dark blue-grey).
const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.02,
    g: 0.03,
    b: 0.05,
    a: 1.0,
};

/// A failure while setting up or recreating the GPU surface.
///
/// There is one variant per failing step.
#[derive(Debug)]
pub enum Error {
    /// The surface could not be created from the window.
    Surface(wgpu::CreateSurfaceError),
    /// No adapter compatible with the surface was found.
    Adapter(wgpu::RequestAdapterError),
    /// The adapter refused to create a device.
    Device(wgpu::RequestDeviceError),
    /// The surface has no configuration compatible with the adapter.
    Unsupported,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Surface(e) => write!(f, "cannot create surface: {e}"),
            Self::Adapter(e) => write!(f, "no usable GPU adapter: {e}"),
            Self::Device(e) => write!(f, "cannot create GPU device: {e}"),
            Self::Unsupported => write!(f, "surface is not supported by the adapter"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Surface(e) => Some(e),
            Self::Adapter(e) => Some(e),
            Self::Device(e) => Some(e),
            Self::Unsupported => None,
        }
    }
}

/// What the caller should do after [`WindowSurface::render`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameStatus {
    /// A frame was drawn and presented.
    Presented,
    /// The window has a zero dimension; nothing was done.
    Idle,
    /// Request another redraw immediately.
    RedrawNow,
    /// Skip this frame and retry after a short delay.
    RedrawLater,
    /// The surface was lost; call [`WindowSurface::recreate_surface`].
    SurfaceLost,
}

/// Owns the wgpu instance, surface, device, queue and surface configuration
/// of one window, and presents a constant-colour frame to it.
///
/// ```no_run
/// # async fn demo(
/// #     display: Box<dyn wgpu::wgt::WgpuHasDisplayHandle>,
/// #     window: impl Into<wgpu::SurfaceTarget<'static>>,
/// # ) -> Result<(), st_render::Error> {
/// let mut surface = st_render::WindowSurface::new(display, window, 800, 600).await?;
/// let status = surface.render(800, 600, || {});
/// assert_eq!(status, st_render::FrameStatus::Presented);
/// # Ok(())
/// # }
/// ```
pub struct WindowSurface {
    instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    adapter_info: wgpu::AdapterInfo,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    /// Whether the previous acquire returned `Validation`.
    last_was_validation: bool,
    /// Validation results not yet reported on stderr.
    suppressed_validation: u64,
}

impl WindowSurface {
    /// Creates the instance, surface, adapter, device and surface
    /// configuration for `target` at the given size (clamped to at least 1).
    ///
    /// The adapter is chosen with the high-performance preference unless the
    /// `WGPU_POWER_PREF` environment variable overrides it.
    ///
    /// # Errors
    ///
    /// Returns the [`Error`] variant of the first step that fails.
    pub async fn new(
        display: Box<dyn wgpu::wgt::WgpuHasDisplayHandle>,
        target: impl Into<wgpu::SurfaceTarget<'static>>,
        width: u32,
        height: u32,
    ) -> Result<Self, Error> {
        let instance = wgpu::Instance::new(
            wgpu::InstanceDescriptor::new_with_display_handle_from_env(display),
        );
        let surface = instance.create_surface(target).map_err(Error::Surface)?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::from_env()
                    .unwrap_or(wgpu::PowerPreference::HighPerformance),
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .map_err(Error::Adapter)?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .map_err(Error::Device)?;
        let mut config = surface
            .get_default_config(&adapter, width.max(1), height.max(1))
            .ok_or(Error::Unsupported)?;
        config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&device, &config);
        Ok(Self {
            instance,
            surface,
            adapter_info: adapter.get_info(),
            device,
            queue,
            config,
            last_was_validation: false,
            suppressed_validation: 0,
        })
    }

    /// Returns information about the adapter that was selected.
    pub fn adapter_info(&self) -> &wgpu::AdapterInfo {
        &self.adapter_info
    }

    /// Draws and presents one frame for a window of the given inner size.
    ///
    /// `pre_present` runs just before the frame is presented (for example
    /// winit's `pre_present_notify`). Returns what the caller should do next.
    pub fn render(&mut self, width: u32, height: u32, pre_present: impl FnOnce()) -> FrameStatus {
        if width == 0 || height == 0 {
            return FrameStatus::Idle;
        }
        if width != self.config.width || height != self.config.height {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
        }
        use wgpu::CurrentSurfaceTexture as T;
        let acquired = self.surface.get_current_texture();
        if matches!(acquired, T::Validation) {
            if self.last_was_validation {
                self.suppressed_validation += 1;
            } else {
                match self.suppressed_validation {
                    0 => eprintln!("wgpu: validation error while acquiring a frame"),
                    n => eprintln!(
                        "wgpu: validation error while acquiring a frame ({n} similar errors suppressed)"
                    ),
                }
                self.suppressed_validation = 0;
            }
            self.last_was_validation = true;
            return FrameStatus::RedrawLater;
        }
        self.last_was_validation = false;
        match acquired {
            T::Success(frame) => {
                self.draw(frame, pre_present);
                FrameStatus::Presented
            }
            T::Suboptimal(frame) => {
                self.draw(frame, pre_present);
                self.surface.configure(&self.device, &self.config);
                FrameStatus::RedrawNow
            }
            T::Outdated => {
                self.surface.configure(&self.device, &self.config);
                FrameStatus::RedrawNow
            }
            T::Timeout | T::Occluded | T::Validation => FrameStatus::RedrawLater,
            T::Lost => FrameStatus::SurfaceLost,
        }
    }

    /// Recreates the surface from a new target, using the kept instance,
    /// device and configuration (wgpu's recovery for a lost surface).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Surface`] if the surface cannot be created.
    pub fn recreate_surface(
        &mut self,
        target: impl Into<wgpu::SurfaceTarget<'static>>,
    ) -> Result<(), Error> {
        self.surface = self
            .instance
            .create_surface(target)
            .map_err(Error::Surface)?;
        self.surface.configure(&self.device, &self.config);
        Ok(())
    }

    fn draw(&self, frame: wgpu::SurfaceTexture, pre_present: impl FnOnce()) {
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(CLEAR_COLOR),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        self.queue.submit(Some(encoder.finish()));
        pre_present();
        self.queue.present(frame);
    }
}
