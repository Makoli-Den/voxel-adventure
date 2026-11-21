//! Game entry point.
//!
//! This is where the layers are wired together: it opens the window, creates the
//! renderer, generates a chunk of terrain and runs the event loop, flying a camera
//! over the terrain. Mods and networking will join later.

mod camera;
mod controls;
mod pacing;

use std::error::Error;
use std::fmt::Display;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::Vec3;
use vox_render::{Renderer, create_renderer};
use vox_world::{build_mesh, generate_terrain};
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use camera::Camera;
use controls::Controls;

/// Seed of the terrain that is generated at startup.
const WORLD_SEED: u32 = 1;
/// How fast the camera flies, in blocks per second.
const MOVE_SPEED: f32 = 12.0;
/// How far the camera turns per pixel of mouse motion, in radians.
const LOOK_SENSITIVITY: f32 = 0.0025;
/// Longest frame time that is taken into account, in seconds. After a stall (a window
/// drag, a breakpoint) the camera must not jump across the world.
const MAX_FRAME_TIME: f32 = 0.1;

/// Application state driven by the window event loop.
struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Box<dyn Renderer>>,
    camera: Camera,
    controls: Controls,
    /// When the previous frame started, to measure how long it took.
    last_frame: Instant,
    /// Time between two frames, derived from the monitor's refresh rate.
    frame_interval: Duration,
    /// When the next frame is due.
    next_frame: Instant,
    /// True while the window has zero size. The loop sleeps instead of spinning.
    minimized: bool,
    /// The error that stopped the game, reported after the event loop ends.
    error: Option<String>,
}

impl App {
    fn new() -> Self {
        Self {
            window: None,
            renderer: None,
            // Above and in front of the chunk, looking at it from a distance and slightly down.
            camera: Camera::new(Vec3::new(16.0, 34.0, 58.0), 0.0, -0.45),
            controls: Controls::default(),
            last_frame: Instant::now(),
            frame_interval: pacing::frame_interval(None),
            next_frame: Instant::now(),
            minimized: false,
            error: None,
        }
    }

    /// Opens the window, creates the renderer and uploads the terrain.
    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<(), Box<dyn Error>> {
        let attributes = Window::default_attributes().with_title("Voxel Adventure");
        let window = Arc::new(event_loop.create_window(attributes)?);
        let size = window.inner_size();
        // Temporary: shaders are read straight from the source tree until the resource
        // virtual file system exists and resolves them through the active packs.
        let shader_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/shaders");
        let mut renderer =
            create_renderer(Arc::clone(&window), size.width, size.height, &shader_dir)?;
        println!("Rendering on: {}", renderer.device_name());
        renderer.set_mesh(&build_mesh(&generate_terrain(WORLD_SEED)))?;
        renderer.set_camera(self.camera.view_projection(size.width, size.height));

        let refresh = window
            .current_monitor()
            .and_then(|monitor| monitor.refresh_rate_millihertz());
        self.frame_interval = pacing::frame_interval(refresh);
        self.window = Some(window);
        self.renderer = Some(renderer);
        self.last_frame = Instant::now();
        self.next_frame = self.last_frame;
        Ok(())
    }

    /// Moves the camera for the time since the last frame and hands it to the renderer.
    fn update_camera(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_frame).as_secs_f32();
        self.last_frame = now;

        let distance = MOVE_SPEED * elapsed.min(MAX_FRAME_TIME);
        self.camera.translate(self.controls.movement(), distance);

        if let (Some(window), Some(renderer)) = (&self.window, self.renderer.as_mut()) {
            let size = window.inner_size();
            renderer.set_camera(self.camera.view_projection(size.width, size.height));
        }
    }

    /// Handles a keyboard event; Escape quits.
    fn handle_key(&mut self, event_loop: &ActiveEventLoop, code: KeyCode, state: ElementState) {
        let pressed = state == ElementState::Pressed;
        if code == KeyCode::Escape && pressed {
            event_loop.exit();
        } else {
            self.controls.set_key(code, pressed);
        }
    }

    /// Remembers the error and asks the event loop to stop.
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: impl Display) {
        self.error = Some(error.to_string());
        event_loop.exit();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            if let Err(error) = self.start(event_loop) {
                self.fail(event_loop, error);
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    self.handle_key(event_loop, code, event.state);
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Right,
                ..
            } => self.controls.set_looking(state == ElementState::Pressed),
            // Held keys would stay "held" forever, because their release goes to another window.
            WindowEvent::Focused(false) => self.controls.release_all(),
            WindowEvent::Resized(size) => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.resize(size.width, size.height);
                }
                // While minimized nothing is drawn and the loop sleeps until the next event;
                // otherwise `about_to_wait` paces the frames.
                self.minimized = size.width == 0 || size.height == 0;
                if self.minimized {
                    event_loop.set_control_flow(ControlFlow::Wait);
                }
            }
            WindowEvent::RedrawRequested => {
                self.update_camera();
                let result = self.renderer.as_mut().map(|r| r.render_frame());
                if let Some(Err(error)) = result {
                    self.fail(event_loop, error);
                }
            }
            _ => {}
        }
    }

    fn device_event(&mut self, _event_loop: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta: (dx, dy) } = event {
            if self.controls.is_looking() {
                self.camera
                    .look(dx as f32 * LOOK_SENSITIVITY, dy as f32 * LOOK_SENSITIVITY);
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.minimized {
            return;
        }
        // Draw at a steady rate and sleep in between, so the processor and the GPU rest
        // and window events are still handled promptly.
        let now = Instant::now();
        if now >= self.next_frame {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            // Counted from now, not from the old deadline: after a stall the game does not
            // try to catch up with a burst of frames.
            self.next_frame = now + self.frame_interval;
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }
}

fn main() -> ExitCode {
    let event_loop = match EventLoop::new() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            eprintln!("cannot create the event loop: {error}");
            return ExitCode::FAILURE;
        }
    };
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = App::new();
    if let Err(error) = event_loop.run_app(&mut app) {
        eprintln!("event loop failed: {error}");
        return ExitCode::FAILURE;
    }
    match app.error {
        Some(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
        None => ExitCode::SUCCESS,
    }
}
