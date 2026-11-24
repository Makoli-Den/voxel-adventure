//! Game entry point.
//!
//! This is where the layers are wired together: it stacks the resource packs, opens the
//! window, creates the renderer with the shaders from the packs, starts the mods' scripts,
//! streams terrain chunks in around the camera and runs the event loop, flying a camera over
//! the terrain and sending the player's block edits to the mods. Networking will join later.

mod camera;
mod cli;
mod content;
mod controls;
mod editing;
mod hud;
mod metrics;
mod mod_scripts;
mod pacing;
mod packs;
mod physics;
mod selection;
mod startup;
mod streaming;

use std::env;
use std::error::Error;
use std::fmt::Display;
use std::io::{self, Write};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::Vec3;
use vox_content::BlockTextures;
use vox_mods::{DiscoveredMod, ModHost, ModStatus};
use vox_render::{
    RenderError, RenderStats, Renderer, ShaderSources, TextureLayers, create_renderer,
};
use vox_world::{BlockRegistry, ChunkMesh, World, raycast};
use winit::application::ApplicationHandler;
use winit::event::{
    DeviceEvent, DeviceId, ElementState, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use camera::{Camera, Movement};
use cli::{Command, Options};
use content::Content;
use controls::Controls;
use editing::{Edit, EditAction, REACH, StaleChunks};
use metrics::FrameHistory;
use pacing::MAX_FRAME_TIME;
use physics::{Body, WalkInput};
use selection::Selection;
use streaming::{ChunkStreamer, Terrain, area, remesh};

/// The window title.
const WINDOW_TITLE: &str = "Voxel Adventure";
/// The exit code for a mistake on the command line.
const EXIT_USAGE: u8 = 2;
/// Seed of the terrain that is generated at startup.
const WORLD_SEED: u32 = 1;
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

/// Timing of the first fill of the area around the camera, for one log line: the time from
/// the start until nothing is queued any more, and the time spent in uploads on the way.
struct FillReport {
    /// When streaming started.
    started: Instant,
    /// Time spent in `set_chunk_mesh` so far.
    upload_time: Duration,
    /// Meshes uploaded so far.
    uploads: u32,
    /// Whether the line has been printed; it is printed once.
    printed: bool,
}

impl FillReport {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            upload_time: Duration::ZERO,
            uploads: 0,
            printed: false,
        }
    }

    /// Adds a frame's uploads.
    fn add_uploads(&mut self, count: usize, took: Duration) {
        self.uploads = self
            .uploads
            .saturating_add(u32::try_from(count).unwrap_or(u32::MAX));
        self.upload_time += took;
    }

    /// The log line, the first time it is asked for after the queue emptied.
    fn line_once(&mut self, streamer: &ChunkStreamer) -> Option<String> {
        let stats = streamer.stats();
        if self.printed || stats.queued > 0 {
            return None;
        }
        self.printed = true;
        let average_job = streamer.average_job_time().unwrap_or_default();
        let average_upload = if self.uploads > 0 {
            self.upload_time / self.uploads
        } else {
            Duration::ZERO
        };
        Some(format!(
            "Streaming: {} chunks loaded in {:.1} ms, average job {:.3} ms, average upload {:.3} ms, {} workers",
            stats.loaded,
            metrics::millis(self.started.elapsed()),
            metrics::millis(average_job),
            metrics::millis(average_upload),
            streamer.worker_count()
        ))
    }
}

/// Application state driven by the window event loop.
struct App {
    options: Options,
    /// The GLSL of every pipeline, read from the packs before the window opens.
    shaders: ShaderSources,
    /// The block textures, read from the packs before the window opens and handed to the
    /// renderer when it is created; `None` after that, so the pixels are not kept twice.
    block_textures: Option<BlockTextures>,
    /// When the app was created, for the `--fly` autopilot's duration.
    run_started: Instant,
    window: Option<Arc<Window>>,
    renderer: Option<Box<dyn Renderer>>,
    /// The terrain every streaming job and every remesh generates from.
    terrain: Arc<Terrain>,
    /// The blocks the player may place and the one the place key puts down.
    selection: Selection,
    /// Every registered block, so the mods can name the blocks of the world.
    registry: BlockRegistry,
    /// The running mod scripts; the player's block edits are sent to them.
    mods: ModHost,
    /// Loads and unloads the terrain around the camera; set up with the renderer.
    streamer: Option<ChunkStreamer>,
    /// Progress of the first fill, for the log line.
    fill: FillReport,
    /// The chunks that are loaded around the camera: what the player's ray hits and edits.
    /// Chunks are added and dropped as streaming loads and unloads them, so it stays as big
    /// as the view area.
    world: World,
    /// Reused for the mesh of every chunk rebuilt after an edit.
    edit_scratch: ChunkMesh,
    camera: Camera,
    /// The player's box. While flying it only follows the camera; while walking it moves the
    /// camera.
    body: Body,
    /// Whether the player walks (collision and gravity) instead of flying; the G key toggles.
    walking: bool,
    controls: Controls,
    /// Whether the frame rate is capped (twice the monitor's refresh rate, see `pacing`).
    /// Off by default, so the metrics show what the game can really do; the L key toggles it.
    frame_limit: bool,
    /// Whether the metrics panel is shown; the F3 key toggles it.
    hud_visible: bool,
    /// When the previous frame started, to measure how long it took.
    last_frame: Instant,
    /// Time between two frames, derived from the monitor's refresh rate.
    frame_interval: Duration,
    /// When the next frame is due.
    next_frame: Instant,
    /// True while the window has zero size. The loop sleeps instead of spinning.
    minimized: bool,
    /// The last ten seconds of frames, for the live figures in the metrics panel.
    live_frames: FrameHistory,
    /// Every frame of the run (up to a limit), for the summary printed at exit.
    run_frames: FrameHistory,
    /// When the metrics panel was last refreshed.
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
    fn new(options: Options, shaders: ShaderSources, content: Content, mods: ModHost) -> Self {
        Self {
            terrain: Arc::new(Terrain::new(
                WORLD_SEED,
                content.terrain,
                content.face_textures(),
            )),
            selection: content.selection,
            registry: content.registry,
            mods,
            options,
            shaders,
            block_textures: Some(content.textures),
            run_started: Instant::now(),
            window: None,
            renderer: None,
            streamer: None,
            fill: FillReport::new(),
            world: World::new(),
            edit_scratch: ChunkMesh::default(),
            camera: Camera::new(CAMERA_START, CAMERA_START_YAW, CAMERA_START_PITCH),
            body: Body::from_eye(CAMERA_START.to_array()),
            walking: false,
            controls: Controls::default(),
            frame_limit: false,
            hud_visible: true,
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

    /// Opens the window, creates the renderer and starts streaming the terrain.
    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<(), Box<dyn Error>> {
        // `--background` opens the window without taking the focus from whatever the
        // player is doing in another window.
        let attributes = Window::default_attributes()
            .with_title(WINDOW_TITLE)
            .with_active(!self.options.background);
        let window = Arc::new(event_loop.create_window(attributes)?);
        let size = window.inner_size();
        let mut renderer =
            create_renderer(Arc::clone(&window), size.width, size.height, &self.shaders)?;
        println!("Rendering on: {}", renderer.device_name());
        if let Some(textures) = self.block_textures.take() {
            renderer
                .set_block_textures(&texture_layers(&textures))
                .map_err(|error| {
                    format!(
                        "cannot use the block textures: {error}; check the packs given with --pack"
                    )
                })?;
        }
        renderer.set_camera(self.camera.view_projection(size.width, size.height));

        // No chunk is loaded here: the first frames stream them in, nearest first.
        self.fill = FillReport::new();
        self.streamer = Some(ChunkStreamer::new(
            Arc::clone(&self.terrain),
            self.options.view_radius,
        ));
        self.window = Some(window);
        self.renderer = Some(renderer);
        self.update_frame_interval();
        self.refresh_hud()?;
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
        let dt = since_last_frame.as_secs_f32().min(MAX_FRAME_TIME);
        // The autopilot always flies, whatever mode the player toggled.
        if self.walking && self.options.fly_seconds.is_none() {
            // Walking: the body decides where the eye is (decision 0012).
            let movement = self.controls.movement();
            let input = WalkInput {
                direction: self.camera.walk_direction(movement),
                jump: movement.up > 0.0,
            };
            self.body.step(&self.world, input, dt);
            self.camera.set_position(Vec3::from(self.body.eye()));
        } else {
            let (movement, speed) = if self.options.fly_seconds.is_some() {
                // The autopilot flies sideways, keeping the height, so the camera always meets
                // terrain that has not been streamed in yet.
                let sideways = Movement {
                    right: 1.0,
                    ..Movement::default()
                };
                (sideways, cli::FLY_SPEED)
            } else {
                (self.controls.movement(), MOVE_SPEED)
            };
            self.camera.translate(movement, speed * dt);
            // The body follows the flying eye, so placing a block still keeps clear of it and
            // switching to walking continues from here.
            self.body = Body::from_eye(self.camera.position().to_array());
        }

        if let (Some(window), Some(renderer)) = (&self.window, self.renderer.as_mut()) {
            // The aspect ratio comes from the window. For a frame or so after a resize the
            // swapchain may still have the old size, which only stretches that one frame.
            let size = window.inner_size();
            renderer.set_camera(self.camera.view_projection(size.width, size.height));
        }
    }

    /// One frame of streaming: loads and unloads chunks around the camera and hands the
    /// finished meshes to the renderer. Removals come first, so their memory is freed before
    /// the uploads need it.
    fn stream(&mut self) -> Result<(), Box<dyn Error>> {
        let (Some(streamer), Some(renderer)) = (self.streamer.as_mut(), self.renderer.as_mut())
        else {
            return Ok(());
        };
        let changes = streamer.update(area::chunk_of(self.camera.position().to_array()))?;
        for pos in &changes.removed {
            renderer.remove_chunk_mesh(*pos);
        }
        editing::apply_streamed(&mut self.world, &changes.removed, changes.loaded);
        let uploading = Instant::now();
        let mut uploaded = 0;
        // An upload error is fatal (the caller exits the game), so the streamer's record of
        // what is loaded need not be corrected when one fails halfway.
        for (pos, mesh) in &changes.uploads {
            renderer.set_chunk_mesh(*pos, mesh)?;
            uploaded += 1;
        }
        if uploaded > 0 {
            self.fill.add_uploads(uploaded, uploading.elapsed());
        }
        if let Some(line) = self.fill.line_once(streamer) {
            println!("{line}");
        }
        Ok(())
    }

    /// Applies the number key and wheel movement since the last frame to the selected block
    /// and shows the new block in the panel.
    fn update_selection(&mut self) -> Result<(), RenderError> {
        let mut changed = false;
        if let Some(slot) = self.controls.take_slot() {
            changed |= self.selection.select_slot(slot);
        }
        let steps = self.controls.take_wheel_steps();
        if steps != 0 {
            changed |= self.selection.cycle(steps);
        }
        if changed {
            self.refresh_hud()?;
        }
        Ok(())
    }

    /// Carries out the edit the player asked for since the last frame, if any: casts a ray
    /// from the eye along the look direction and applies the edit the ray picks.
    ///
    /// The ray is only cast when an edit was asked for, since nothing shows the targeted block.
    fn edit(&mut self) -> Result<(), RenderError> {
        let Some(action) = self.controls.take_edit() else {
            return Ok(());
        };
        let eye = self.camera.position().to_array();
        let direction = self.camera.forward().to_array();
        let Some(hit) = raycast(&self.world, eye, direction, REACH) else {
            return Ok(());
        };
        let Some(edit) = editing::plan(action, &hit, &self.body, self.selection.selected()) else {
            return Ok(());
        };
        self.apply_edit(action, edit)
    }

    /// Writes the player's `edit`, sends the event to the mods, writes what they asked for and
    /// rebuilds the mesh of every chunk the writes made out of date, each once (decision 0014).
    /// The mods see the world after the player's write and cannot undo it.
    fn apply_edit(&mut self, action: EditAction, edit: Edit) -> Result<(), RenderError> {
        // `None` when the chunk of the cell is not loaded, for example above the terrain layer.
        let Some(previous) = self.world.block_at(edit.position) else {
            return Ok(());
        };
        let Some(changed) = self.world.set_block(edit.position, edit.block) else {
            return Ok(());
        };
        if changed.is_empty() {
            // The cell already held the block: nothing happened, so there is no event.
            return Ok(());
        }
        let mut stale = StaleChunks::default();
        stale.add(&changed);
        let event = editing::block_event(action, &edit, previous);
        let mod_edits = self.mods.block_event(&event, &self.world, &self.registry);
        editing::apply_mod_edits(&mut self.world, &mod_edits, &mut stale);
        let Some(renderer) = self.renderer.as_mut() else {
            return Ok(());
        };
        for pos in stale.as_slice() {
            if remesh(&self.terrain, &self.world, *pos, &mut self.edit_scratch) {
                // An empty mesh removes the chunk's entry, so a chunk that was dug empty vanishes.
                renderer.set_chunk_mesh(*pos, &self.edit_scratch)?;
            }
        }
        Ok(())
    }

    /// Updates the camera, streams chunks, draws one frame and records how long it all took.
    ///
    /// The recorded work time covers moving the camera, streaming and the renderer's
    /// `render_frame` (which includes waiting for the GPU), but not refreshing the metrics
    /// panel; that cost shows up in the next frame's interval.
    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        let started = Instant::now();
        let interval = started.duration_since(self.last_frame);
        self.last_frame = started;

        self.update_camera(interval);
        if let Some(seconds) = self.options.fly_seconds {
            if self.run_started.elapsed().as_secs_f32() >= seconds {
                event_loop.exit();
                return;
            }
        }
        if let Err(error) = self.stream() {
            self.fail(event_loop, error);
            return;
        }
        // Before the edit, so a block chosen and placed in the same frame is the chosen one.
        if let Err(error) = self.update_selection() {
            self.fail(event_loop, error);
            return;
        }
        // After streaming, so the world holds the chunks that were just loaded.
        if let Err(error) = self.edit() {
            self.fail(event_loop, error);
            return;
        }
        print_mod_messages(&mut self.mods);
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
        if let Err(error) = self.record_frame(interval, started.elapsed()) {
            self.fail(event_loop, error);
        }
    }

    /// Adds a finished frame to the metrics and refreshes the metrics panel now and then.
    fn record_frame(&mut self, interval: Duration, work: Duration) -> Result<(), RenderError> {
        if self.skip_next_sample {
            self.skip_next_sample = false;
            return Ok(());
        }
        let gpu = self.render_stats.gpu_time;
        self.live_frames.push(interval, work, gpu);
        self.run_frames.push(interval, work, gpu);

        if self.last_frame.duration_since(self.last_report) >= metrics::REFRESH_INTERVAL {
            self.last_report = self.last_frame;
            self.refresh_hud()?;
        }
        Ok(())
    }

    /// Hands the renderer the current metrics panel, or removes it while it is hidden.
    fn refresh_hud(&mut self) -> Result<(), RenderError> {
        let Some(renderer) = self.renderer.as_mut() else {
            return Ok(());
        };
        let mut rects = if self.hud_visible {
            let summary = self.live_frames.summary();
            let stream_stats = self
                .streamer
                .as_ref()
                .map(ChunkStreamer::stats)
                .unwrap_or_default();
            hud::panel(&hud::lines(
                summary.as_ref(),
                &self.render_stats,
                &stream_stats,
                self.frame_limit,
                self.selection.selected_name(),
            ))
        } else {
            Vec::new()
        };
        // The crosshair is always visible, also while the metrics panel is hidden.
        if let Some(window) = &self.window {
            let size = window.inner_size();
            rects.extend(hud::crosshair(size.width, size.height));
        }
        renderer.set_overlay(&rects)
    }

    /// The figures of the whole run, for the line printed when the game ends.
    fn run_summary(&self) -> Option<String> {
        let summary = self.run_frames.summary()?;
        Some(format!(
            "Run summary (frames: {}, play time: {:.1?}): {summary}",
            summary.frames, summary.covered
        ))
    }

    /// Handles a keyboard event: Escape quits, L toggles the frame limit, F3 shows or hides
    /// the metrics panel, and the movement keys go to the controls. The toggles act on the
    /// press only, not on the repeats of a held key.
    fn handle_key(
        &mut self,
        event_loop: &ActiveEventLoop,
        code: KeyCode,
        state: ElementState,
        repeat: bool,
    ) {
        let pressed = state == ElementState::Pressed;
        match code {
            KeyCode::Escape if pressed => event_loop.exit(),
            KeyCode::KeyL if pressed && !repeat => {
                self.frame_limit = !self.frame_limit;
                self.next_frame = Instant::now();
                self.refresh_after_toggle(event_loop);
            }
            KeyCode::KeyF if pressed && !repeat => self.controls.request_edit(EditAction::Place),
            // The block choice acts on the press only; a held key must not keep selecting.
            code if controls::is_slot_key(code) => {
                if pressed && !repeat {
                    self.controls.request_slot(code);
                }
            }
            KeyCode::KeyG if pressed && !repeat => {
                self.toggle_walking();
            }
            KeyCode::F3 if pressed && !repeat => {
                self.hud_visible = !self.hud_visible;
                self.refresh_after_toggle(event_loop);
            }
            _ => self.controls.set_key(code, pressed),
        }
    }

    /// Switches between flying and walking. Walking continues from where the camera is, at
    /// rest, and is refused while the box would be inside solid blocks (the player could not
    /// get out by walking); flying is always allowed.
    fn toggle_walking(&mut self) {
        if self.walking {
            self.walking = false;
            return;
        }
        let body = Body::from_eye(self.camera.position().to_array());
        if body.is_free(&self.world) {
            self.body = body;
            self.walking = true;
        }
    }

    /// Shows the effect of a toggle in the panel at once instead of at the next refresh.
    fn refresh_after_toggle(&mut self, event_loop: &ActiveEventLoop) {
        if let Err(error) = self.refresh_hud() {
            self.fail(event_loop, error);
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
                    self.handle_key(event_loop, code, event.state, event.repeat);
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Right,
                ..
            } => self.controls.set_looking(state == ElementState::Pressed),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.controls.click(),
            WindowEvent::MouseWheel { delta, .. } => match delta {
                MouseScrollDelta::LineDelta(_, lines) => self.controls.scroll(lines),
                MouseScrollDelta::PixelDelta(pixels) => self.controls.scroll_pixels(pixels.y),
            },
            // Held keys would stay "held" forever, because their release goes to another window.
            WindowEvent::Focused(false) => self.controls.release_all(),
            WindowEvent::Focused(true) => self.controls.focus_gained(),
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
                } else {
                    // The crosshair belongs in the middle of the new size.
                    self.refresh_after_toggle(event_loop);
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
        self.controls.end_of_events();
        if self.minimized {
            return;
        }
        if !self.frame_limit {
            // No limit: ask for the next frame at once and never sleep, so the metrics show
            // what the game can really do. The GPU runs flat out.
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            event_loop.set_control_flow(ControlFlow::Poll);
            return;
        }
        // Limited: draw at a steady rate and sleep in between, so the processor and the GPU
        // rest and window events are still handled promptly.
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

/// Prints the messages waiting in `mods` to the error output, each tagged with its mod's id.
/// A closed error output is not a reason to stop the game (`eprintln!` would panic), so a
/// write error is ignored.
fn print_mod_messages(mods: &mut ModHost) {
    let mut output = io::stderr().lock();
    for message in mods.take_messages() {
        let _ = writeln!(output, "{}", mod_scripts::message_line(&message));
    }
}

/// Starts the scripts of `mods` (in load order), prints how each start ended, then what the
/// scripts printed while they ran. A failing script only disables its own mod.
fn start_mods(mods: &[DiscoveredMod]) -> ModHost {
    let (mut host, statuses) = ModHost::start(mods);
    for status in &statuses {
        let line = mod_scripts::status_line(status, mods);
        match status {
            ModStatus::Loaded { .. } => println!("{line}"),
            ModStatus::Disabled { .. } => eprintln!("warning: {line}"),
        }
    }
    print_mod_messages(&mut host);
    host
}

/// The renderer's borrowed view of the block textures the content loader built.
fn texture_layers(textures: &BlockTextures) -> TextureLayers<'_> {
    TextureLayers {
        size: textures.size,
        layer_count: textures.layer_count,
        pixels: &textures.pixels,
    }
}

fn main() -> ExitCode {
    let options = match cli::parse(env::args().skip(1)) {
        Ok(Command::Run(options)) => options,
        Ok(Command::Help) => {
            println!("{}", cli::usage());
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("{message}\n\n{}", cli::usage());
            return ExitCode::from(EXIT_USAGE);
        }
    };
    // Everything is read before the window opens, so a missing shader or a broken blocks file
    // is reported without flashing one.
    let startup = match startup::assemble(
        &startup::mod_dirs(&options.mods),
        packs::built_in_pack(),
        &options.packs,
    ) {
        Ok(startup) => startup,
        Err(errors) => {
            for error in errors {
                eprintln!("error: {error}");
            }
            return ExitCode::FAILURE;
        }
    };
    for root in &startup.missing_packs {
        eprintln!(
            "warning: the pack {} is not a directory, so it provides nothing",
            packs::describe_root(root)
        );
    }
    println!("{}", startup.load_order_line());
    let shaders = match packs::load_shaders(&startup.stack) {
        Ok(shaders) => shaders,
        Err(error) => {
            eprintln!("error: cannot load the shaders: {error}");
            return ExitCode::FAILURE;
        }
    };
    let content = match Content::new(&startup.stack, &startup.blocks_files) {
        Ok(content) => content,
        Err(error) => {
            eprintln!("error: cannot load the blocks: {error}");
            return ExitCode::FAILURE;
        }
    };
    for warning in &content.warnings {
        eprintln!("warning: {warning}");
    }
    println!(
        "Content: {} blocks, {} textures",
        content.registry.len(),
        content.textures.layer_count
    );
    let event_loop = match EventLoop::new() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            eprintln!("cannot create the event loop: {error}");
            return ExitCode::FAILURE;
        }
    };
    event_loop.set_control_flow(ControlFlow::Poll);

    let mods = start_mods(&startup.mods);
    let mut app = App::new(options, shaders, content, mods);
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
