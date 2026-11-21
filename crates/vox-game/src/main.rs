//! Game entry point.
//!
//! This is where the layers are wired together: it opens the window, creates the
//! renderer, generates a chunk of terrain and runs the event loop, flying a camera
//! over the terrain. Mods and networking will join later.

mod camera;
mod controls;
mod metrics;
mod pacing;
mod world_loader;

use std::error::Error;
use std::fmt::Display;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::Vec3;
use vox_render::{RenderStats, Renderer, create_renderer};
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use camera::Camera;
use controls::Controls;
use metrics::FrameHistory;

/// The name of the game, at the start of the window title.
const WINDOW_TITLE: &str = "Voxel Adventure";
/// Seed of the terrain that is generated at startup.
const WORLD_SEED: u32 = 1;
/// How far the loaded area reaches from the origin chunk along X and Z, in chunks. The
/// camera's far plane (`FAR` in `camera.rs`) has to cover the area, so the two go together.
const VIEW_RADIUS: i32 = 8;
/// Where the camera starts: above the loaded area, back from the origin chunk (the terrain
/// lies at heights 8 to 24), so a large part of the area is in view.
const CAMERA_START: Vec3 = Vec3::new(16.0, 60.0, 90.0);
/// The camera's start yaw in radians; 0 looks along -Z, towards the loaded area.
const CAMERA_START_YAW: f32 = 0.0;
/// The camera's start pitch in radians; negative looks down at the terrain.
const CAMERA_START_PITCH: f32 = -0.35;
/// How fast the camera flies, in blocks per second. The loaded area is hundreds of blocks
/// across, so this is brisk.
const MOVE_SPEED: f32 = 40.0;
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
    /// The last ten seconds of frames, for the live figures in the window title.
    live_frames: FrameHistory,
    /// Every frame of the run (up to a limit), for the summary printed at exit.
    run_frames: FrameHistory,
    /// When the window title was last updated with the live figures.
    last_report: Instant,
    /// What the renderer reported after the latest frame.
    render_stats: RenderStats,
    /// Set when the window was minimized: the first frame after it follows a pause, not a
    /// frame time, so it is not recorded.
    skip_next_sample: bool,
    /// The error that stopped the game, reported after the event loop ends.
    error: Option<String>,
}

impl App {
    fn new() -> Self {
        Self {
            window: None,
            renderer: None,
            camera: Camera::new(CAMERA_START, CAMERA_START_YAW, CAMERA_START_PITCH),
            controls: Controls::default(),
            last_frame: Instant::now(),
            frame_interval: pacing::frame_interval(None),
            next_frame: Instant::now(),
            minimized: false,
            live_frames: FrameHistory::live(),
            run_frames: FrameHistory::whole_run(),
            last_report: Instant::now(),
            render_stats: RenderStats::default(),
            skip_next_sample: false,
            error: None,
        }
    }

    /// Opens the window, creates the renderer and uploads the terrain.
    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<(), Box<dyn Error>> {
        let attributes = Window::default_attributes().with_title(WINDOW_TITLE);
        let window = Arc::new(event_loop.create_window(attributes)?);
        let size = window.inner_size();
        // Temporary: shaders are read straight from the source tree until the resource
        // virtual file system exists and resolves them through the active packs.
        let shader_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/shaders");
        let mut renderer =
            create_renderer(Arc::clone(&window), size.width, size.height, &shader_dir)?;
        println!("Rendering on: {}", renderer.device_name());
        let loaded = world_loader::load_area(WORLD_SEED, VIEW_RADIUS);
        let uploading = Instant::now();
        for (pos, mesh) in &loaded.meshes {
            renderer.set_chunk_mesh(*pos, mesh)?;
        }
        println!(
            "World: {}, uploaded in {:.1?}",
            loaded.stats,
            uploading.elapsed()
        );
        renderer.set_camera(self.camera.view_projection(size.width, size.height));

        self.window = Some(window);
        self.renderer = Some(renderer);
        self.update_frame_interval();
        self.last_frame = Instant::now();
        self.next_frame = self.last_frame;
        self.last_report = self.last_frame;
        Ok(())
    }

    /// Reads the refresh rate of the monitor the window is on and sets the frame interval
    /// from it. Called again when the window moves, since the other monitor may differ.
    fn update_frame_interval(&mut self) {
        let Some(window) = &self.window else {
            return;
        };
        let refresh = window
            .current_monitor()
            .and_then(|monitor| monitor.refresh_rate_millihertz());
        self.frame_interval = pacing::frame_interval(refresh);
    }

    /// Moves the camera for the time since the last frame and hands it to the renderer.
    fn update_camera(&mut self, since_last_frame: Duration) {
        let distance = MOVE_SPEED * since_last_frame.as_secs_f32().min(MAX_FRAME_TIME);
        self.camera.translate(self.controls.movement(), distance);

        if let (Some(window), Some(renderer)) = (&self.window, self.renderer.as_mut()) {
            // The aspect ratio comes from the window. For a frame or so after a resize the
            // swapchain may still have the old size, which only stretches that one frame.
            let size = window.inner_size();
            renderer.set_camera(self.camera.view_projection(size.width, size.height));
        }
    }

    /// Updates the camera, draws one frame and records how long it all took.
    ///
    /// The recorded work time covers moving the camera and the renderer's `render_frame`
    /// (which includes waiting for the GPU), but not updating the window title; that cost
    /// shows up in the next frame's interval.
    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        let started = Instant::now();
        let interval = started.duration_since(self.last_frame);
        self.last_frame = started;

        self.update_camera(interval);
        let result = self
            .renderer
            .as_mut()
            .map(|renderer| renderer.render_frame());
        if let Some(Err(error)) = result {
            self.fail(event_loop, error);
            return;
        }
        if let Some(renderer) = &self.renderer {
            self.render_stats = renderer.stats();
        }
        self.record_frame(interval, started.elapsed());
    }

    /// Adds a finished frame to the metrics and refreshes the window title now and then.
    fn record_frame(&mut self, interval: Duration, work: Duration) {
        if self.skip_next_sample {
            self.skip_next_sample = false;
            return;
        }
        let gpu = self.render_stats.gpu_time;
        self.live_frames.push(interval, work, gpu);
        self.run_frames.push(interval, work, gpu);

        if self.last_frame.duration_since(self.last_report) >= metrics::REFRESH_INTERVAL {
            self.last_report = self.last_frame;
            self.show_live_figures();
        }
    }

    /// Puts the live frame figures into the window title.
    fn show_live_figures(&self) {
        if let (Some(window), Some(summary)) = (&self.window, self.live_frames.summary()) {
            let drawn = &self.render_stats;
            window.set_title(&format!(
                "{WINDOW_TITLE} | {summary} | {} chunks, {} triangles",
                drawn.chunks_drawn, drawn.triangles
            ));
        }
    }

    /// The figures of the whole run, for the line printed when the game ends.
    fn run_summary(&self) -> Option<String> {
        let summary = self.run_frames.summary()?;
        Some(format!(
            "Run summary (frames: {}, play time: {:.1?}): {summary}",
            summary.frames, summary.covered
        ))
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
            WindowEvent::Moved(_) => self.update_frame_interval(),
            WindowEvent::Resized(size) => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.resize(size.width, size.height);
                }
                // While minimized nothing is drawn and the loop sleeps until the next event;
                // otherwise `about_to_wait` paces the frames.
                self.minimized = size.width == 0 || size.height == 0;
                if self.minimized {
                    event_loop.set_control_flow(ControlFlow::Wait);
                    self.skip_next_sample = true;
                }
            }
            WindowEvent::RedrawRequested => self.redraw(event_loop),
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
    let result = event_loop.run_app(&mut app);
    // The summary is printed however the loop ended. A closed standard output is not a
    // reason to fail (`println!` would panic), so a write error is ignored.
    if let Some(summary) = app.run_summary() {
        let _ = writeln!(io::stdout(), "{summary}");
    }
    if let Err(error) = result {
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
