//! Game entry point.
//!
//! This is where the layers are wired together: it stacks the resource packs, opens the
//! window, creates the renderer with the shaders from the packs, starts the mods' scripts,
//! streams terrain chunks in around the camera and runs the event loop, flying a camera over
//! the terrain and sending the player's block edits to the mods. Networking will join later.
//!
//! The mouse is captured while the player plays; Esc, losing the window focus or the menu's
//! buttons switch the input mode (the rules live in `input_mode`, this file only applies them
//! to the window).

mod camera;
mod cli;
mod content;
mod controls;
mod editing;
mod hud;
mod input_mode;
mod metrics;
mod mod_scripts;
mod pacing;
mod packs;
mod physics;
mod selection;
mod startup;
mod streaming;
mod view_distance;

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
    CameraView, DepthMode, Fog, RenderError, RenderStats, Renderer, ShaderSources, TextureLayers,
    create_renderer,
};
use vox_world::{BlockId, BlockRegistry, ChunkMesh, TilePos, World, raycast_with};
use winit::application::ApplicationHandler;
use winit::event::{
    DeviceEvent, DeviceId, ElementState, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Window, WindowId};

use camera::{Camera, Movement};
use cli::{Command, Options};
use content::Content;
use controls::Controls;
use editing::{Edit, EditAction, EditedChunks, REACH};
use input_mode::{InputMode, MenuButton, Outcome};
use metrics::FrameHistory;
use pacing::MAX_FRAME_TIME;
use physics::{Body, Space, WalkInput};
use selection::Selection;
use streaming::{ChunkStreamer, ColumnSpans, StreamStats, Terrain, area, remesh};

/// The window title.
const WINDOW_TITLE: &str = "Voxel Adventure";
/// The exit code for a mistake on the command line.
const EXIT_USAGE: u8 = 2;
/// Seed of the terrain that is generated at startup.
const WORLD_SEED: u32 = 1;
/// How far above the spawn point's ground the camera starts, in blocks: high enough to see the
/// land around it (and the trees, up to 9 blocks tall), low enough to stay among the hills.
const CAMERA_START_HEIGHT: f32 = 30.0;
/// How far behind the spawn point (towards +Z) the camera starts, in blocks, so it looks at
/// the land in front of the spawn from a little way back.
const CAMERA_START_BACK: f32 = 30.0;

/// Where the camera starts: above the land at the spawn point of `seed` (decision 0017), so the
/// player always begins over land, whatever the seed.
fn camera_start(seed: u32) -> Vec3 {
    let [x, y, z] = vox_world::spawn_point(seed);
    Vec3::new(
        x as f32 + 0.5,
        y as f32 + CAMERA_START_HEIGHT,
        z as f32 + 0.5 + CAMERA_START_BACK,
    )
}
/// The camera's start yaw in radians; 0 looks along -Z, towards the loaded area.
const CAMERA_START_YAW: f32 = 0.0;
/// The camera's start pitch in radians; negative looks down at the terrain.
const CAMERA_START_PITCH: f32 = -0.35;
/// How fast the camera flies, in blocks per second. The loaded area is hundreds of blocks
/// across, so this is brisk.
const MOVE_SPEED: f32 = 40.0;
/// How far the camera turns per pixel of mouse motion, in radians.
const LOOK_SENSITIVITY: f32 = 0.0025;

/// Bytes in a mebibyte, for the memory figures of the log lines.
const BYTES_PER_MIB: f64 = 1024.0 * 1024.0;

/// Timing of the first fill of the area and the horizon around the camera, for one log line:
/// the time from the start until nothing is queued any more, and the time spent in uploads on
/// the way. After that, how often columns or tiles were still waiting, the cheap stand-in for
/// holes in the ground until step S8 measures the uncovered area.
struct FillReport {
    /// When streaming started.
    started: Instant,
    /// Time spent in `set_tile_mesh` so far.
    upload_time: Duration,
    /// Meshes uploaded so far.
    uploads: u32,
    /// Whether the line has been printed; it is printed once.
    printed: bool,
    /// Frames since the first fill.
    frames_after_fill: u32,
    /// Of those, frames with columns or tiles waiting (a hole, or new ground at the edge).
    frames_waiting: u32,
    /// The most columns and tiles waiting in one frame after the first fill.
    most_waiting: usize,
}

impl FillReport {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            upload_time: Duration::ZERO,
            uploads: 0,
            printed: false,
            frames_after_fill: 0,
            frames_waiting: 0,
            most_waiting: 0,
        }
    }

    /// Counts a frame after the first fill and whether columns or tiles were waiting in it.
    fn add_frame(&mut self, stats: &StreamStats) {
        if !self.printed {
            return;
        }
        self.frames_after_fill = self.frames_after_fill.saturating_add(1);
        let waiting = stats.waiting();
        if waiting > 0 {
            self.frames_waiting = self.frames_waiting.saturating_add(1);
        }
        self.most_waiting = self.most_waiting.max(waiting);
    }

    /// The figures since the first fill, for the run summary.
    fn waiting_summary(&self) -> String {
        if !self.printed {
            return "first fill not reached".to_owned();
        }
        format!(
            "after the first fill {} of {} frames had columns or tiles waiting (most {})",
            self.frames_waiting, self.frames_after_fill, self.most_waiting
        )
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
        if self.printed || stats.waiting() > 0 {
            return None;
        }
        self.printed = true;
        let average_job = streamer.average_job_time().unwrap_or_default();
        let average_tile = streamer.average_tile_time().unwrap_or_default();
        let average_upload = if self.uploads > 0 {
            self.upload_time / self.uploads
        } else {
            Duration::ZERO
        };
        Some(format!(
            "Streaming: {} columns and {} tiles loaded in {:.1} ms ({} meshes, {:.1} MiB), average job {:.3} ms column / {:.3} ms tile, average upload {:.3} ms, {} workers",
            stats.loaded,
            stats.tiles_loaded,
            metrics::millis(self.started.elapsed()),
            self.uploads,
            stats.mesh_bytes as f64 / BYTES_PER_MIB,
            metrics::millis(average_job),
            metrics::millis(average_tile),
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
    /// The span of every loaded column: what physics and editing know of the layers that are
    /// not in `world`. Updated together with `world` from the streaming changes.
    spans: ColumnSpans,
    /// The liquid block (water): the body passes through it and the edit ray does not stop at
    /// it, so the player can swim and build under water.
    liquid: BlockId,
    /// The chunks the player wrote into, whose neighbours must be completed when they stream
    /// in again (decision 0015).
    edited: EditedChunks,
    /// Reused for the mesh of every chunk rebuilt after an edit.
    edit_scratch: ChunkMesh,
    camera: Camera,
    /// The horizon in blocks the game streams tiles to: the option, lowered for the mesh
    /// budget and the depth buffer when the renderer starts (0: no tiles).
    horizon: i32,
    /// The fog, set from the view radius and the horizon: it ends where the drawn terrain may
    /// end.
    fog: Fog,
    /// Distance of the camera's far plane in blocks, just beyond the fog end.
    far_plane: f32,
    /// How the renderer maps depth, so the projection matches it; read from the renderer
    /// when it is created.
    depth_mode: DepthMode,
    /// The player's box. While flying it only follows the camera; while walking it moves the
    /// camera.
    body: Body,
    /// Whether the player walks (collision and gravity) instead of flying; the G key toggles.
    walking: bool,
    controls: Controls,
    /// Whether the game or the pause menu has the input, and the menu's selection.
    input_mode: InputMode,
    /// The last position of the mouse cursor in window pixels, for clicks on the menu.
    cursor: (f32, f32),
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
        let input_mode = InputMode::new(input_mode::starts_awaiting(&options));
        // Without tiles until `start` knows what the renderer's depth buffer allows.
        let fog = view_distance::fog(options.view_radius, 0);
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
            spans: ColumnSpans::default(),
            liquid: content.terrain.water,
            edited: EditedChunks::default(),
            edit_scratch: ChunkMesh::default(),
            camera: Camera::new(
                camera_start(WORLD_SEED),
                CAMERA_START_YAW,
                CAMERA_START_PITCH,
            ),
            horizon: 0,
            fog,
            far_plane: view_distance::far_plane(fog),
            // A placeholder: `start` overwrites it from the renderer before the first
            // `set_camera`, and no projection is built before the renderer exists.
            depth_mode: DepthMode::Standard,
            body: Body::from_eye(camera_start(WORLD_SEED).to_array()),
            walking: false,
            controls: Controls::default(),
            input_mode,
            cursor: (0.0, 0.0),
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
        self.depth_mode = renderer.depth_mode();
        let horizon = view_distance::effective_horizon(
            self.options.horizon,
            self.options.view_radius,
            self.depth_mode.max_horizon(),
        );
        if let Some(note) =
            view_distance::horizon_note(self.options.horizon, self.options.view_radius, horizon)
        {
            eprintln!("{note}");
        }
        self.horizon = horizon.blocks;
        self.fog = view_distance::fog(self.options.view_radius, self.horizon);
        self.far_plane = view_distance::far_plane(self.fog);
        renderer.set_fog(self.fog);
        renderer.set_camera(&self.camera_view(size.width, size.height));

        // No chunk is loaded here: the first frames stream them in, nearest first.
        self.fill = FillReport::new();
        self.streamer = Some(ChunkStreamer::new(
            Arc::clone(&self.terrain),
            self.options.view_radius,
            self.horizon,
        ));
        self.window = Some(window);
        self.renderer = Some(renderer);
        self.apply_cursor();
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
            // Walking: the body decides where the eye is (decision 0012). While the menu is
            // open the body stands still, so the player does not fall (or keep walking)
            // while paused.
            if !self.input_mode.is_paused() {
                let movement = self.controls.movement();
                let input = WalkInput {
                    direction: self.camera.walk_direction(movement),
                    jump: movement.up > 0.0,
                };
                let space = Space {
                    world: &self.world,
                    spans: &self.spans,
                    liquid: self.liquid,
                };
                self.body.step(space, input, dt);
                self.camera.set_position(Vec3::from(self.body.eye()));
            }
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
            self.camera
                .clamp_height(camera::FLY_FLOOR, camera::FLY_CEILING);
            // The body follows the flying eye, so placing a block still keeps clear of it and
            // switching to walking continues from here.
            self.body = Body::from_eye(self.camera.position().to_array());
        }

        // The aspect ratio comes from the window. For a frame or so after a resize the
        // swapchain may still have the old size, which only stretches that one frame.
        let Some(size) = self.window.as_ref().map(|window| window.inner_size()) else {
            return;
        };
        let view = self.camera_view(size.width, size.height);
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.set_camera(&view);
        }
    }

    /// What the renderer needs of the camera for a window of `width` by `height` pixels:
    /// the matrix around the eye with the far plane just beyond the fog, in the renderer's
    /// depth mode, and the eye.
    fn camera_view(&self, width: u32, height: u32) -> CameraView {
        CameraView {
            view_projection: self.camera.view_projection(
                width,
                height,
                self.far_plane,
                self.depth_mode,
            ),
            position: self.camera.position().to_array(),
        }
    }

    /// One frame of streaming: loads and unloads chunk columns and tiles around the camera and
    /// hands the finished meshes to the renderer. Removals come first, so their memory is freed
    /// before the uploads need it. An unloaded column takes every chunk of it along, also those
    /// an edit created; a streamed chunk never replaces one already in the world, and neither
    /// does its mesh. Tiles of level 1 or more have keys of their own level, so they never
    /// meet a chunk's mesh in the renderer.
    fn stream(&mut self) -> Result<(), Box<dyn Error>> {
        let (Some(streamer), Some(renderer)) = (self.streamer.as_mut(), self.renderer.as_mut())
        else {
            return Ok(());
        };
        let changes = streamer.update(area::chunk_of(self.camera.position().to_array()))?;
        for column in &changes.removed {
            for pos in column.chunks() {
                renderer.remove_tile_mesh(TilePos::chunk(pos));
            }
        }
        for tile in &changes.removed_tiles {
            renderer.remove_tile_mesh(*tile);
        }
        let arrived: Vec<_> = changes.loaded.iter().map(|loaded| loaded.column).collect();
        let kept = editing::apply_streamed(
            &mut self.world,
            &mut self.spans,
            &mut self.edited,
            &changes.removed,
            changes.loaded,
        );
        let uploading = Instant::now();
        let mut uploaded = 0;
        // An upload error is fatal (the caller exits the game), so the streamer's record of
        // what is loaded need not be corrected when one fails halfway.
        for (pos, mesh) in changes
            .uploads
            .iter()
            .filter(|(pos, _)| !kept.contains(pos))
        {
            renderer.set_tile_mesh(TilePos::chunk(*pos), mesh)?;
            uploaded += 1;
        }
        for (tile, mesh) in &changes.tile_uploads {
            renderer.set_tile_mesh(*tile, mesh)?;
            uploaded += 1;
        }
        if uploaded > 0 {
            self.fill.add_uploads(uploaded, uploading.elapsed());
        }
        // After the uploads, so a remeshed neighbour that arrived in this frame keeps the
        // remeshed version.
        let beside = editing::create_chunks_beside_edits(
            &mut self.world,
            &self.terrain,
            &self.spans,
            &self.edited,
            &arrived,
        );
        for pos in beside {
            if remesh(
                &self.terrain,
                &self.world,
                &self.spans,
                pos,
                &mut self.edit_scratch,
            ) {
                renderer.set_tile_mesh(TilePos::chunk(pos), &self.edit_scratch)?;
            }
        }
        if let Some(line) = self.fill.line_once(streamer) {
            println!("{line}");
        }
        self.fill.add_frame(&streamer.stats());
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
        // Missing chunks above a loaded column's span are air, so a ray from the flying height
        // reaches the terrain (decision 0015).
        let lookup = |block| editing::ray_sees(&self.world, &self.spans, self.liquid, block);
        let Some(hit) = raycast_with(lookup, eye, direction, REACH) else {
            return Ok(());
        };
        let Some(edit) = editing::plan(action, &hit, &self.body, self.selection.selected()) else {
            return Ok(());
        };
        self.apply_edit(action, edit)
    }

    /// Writes the player's `edit` (after creating the chunks under the terrain it needs), sends
    /// the event to the mods, writes what they asked for and rebuilds the mesh of every chunk
    /// the writes made out of date, each once (decisions 0014 and 0015). The mods see the world
    /// after the player's write and cannot undo it. An edit that created chunks logs its cost
    /// (decision 0015 budgets it).
    fn apply_edit(&mut self, action: EditAction, edit: Edit) -> Result<(), RenderError> {
        if self.renderer.is_none() {
            return Ok(());
        }
        let started = Instant::now();
        let Some(done) = editing::player_edit(
            &mut self.world,
            &self.terrain,
            &self.spans,
            &mut self.edited,
            action,
            &edit,
        ) else {
            return Ok(());
        };
        let mut stale = done.stale;
        let mod_edits = self
            .mods
            .block_event(&done.event, &self.world, &self.registry);
        let mod_created = editing::apply_mod_edits(
            &mut self.world,
            &self.terrain,
            &self.spans,
            &mut self.edited,
            &mod_edits,
            &mut stale,
        );
        let Some(renderer) = self.renderer.as_mut() else {
            return Ok(());
        };
        for pos in stale.as_slice() {
            if remesh(
                &self.terrain,
                &self.world,
                &self.spans,
                *pos,
                &mut self.edit_scratch,
            ) {
                // An empty mesh removes the chunk's entry, so a chunk that was dug empty vanishes.
                renderer.set_tile_mesh(TilePos::chunk(*pos), &self.edit_scratch)?;
            }
        }
        let created = done.created + mod_created;
        if created > 0 {
            println!(
                "Edit: {created} chunks created, {} remeshed in {:.3} ms",
                stale.as_slice().len(),
                metrics::millis(started.elapsed())
            );
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
        if let Some(window) = &self.window {
            let size = window.inner_size();
            if self.input_mode.is_paused() {
                // The menu covers the crosshair, which means nothing while nothing can be edited.
                rects.extend(hud::menu(
                    size.width,
                    size.height,
                    self.input_mode.selected(),
                ));
            } else {
                // The crosshair is always visible, also while the metrics panel is hidden.
                rects.extend(hud::crosshair(size.width, size.height));
            }
        }
        renderer.set_overlay(&rects)
    }

    /// The menu button under the mouse cursor, if the menu is open and the cursor is on one.
    fn button_under_cursor(&self) -> Option<MenuButton> {
        let window = self.window.as_ref()?;
        if !self.input_mode.is_paused() {
            return None;
        }
        let size = window.inner_size();
        hud::button_at(
            &hud::menu_buttons(size.width, size.height),
            self.cursor.0,
            self.cursor.1,
        )
    }

    /// Hides and captures the cursor while playing, and frees and shows it otherwise.
    ///
    /// Capturing is tried as `Locked` first: on Windows winit implements it by clipping the
    /// cursor to a one-pixel rectangle, and it succeeds there. `Confined` (the cursor stays
    /// inside the window) is the fallback for platforms that refuse `Locked`. Failures are
    /// logged, since a failed capture costs only the capture. Idempotent, so it is safe to
    /// call again whenever the state may have drifted.
    fn apply_cursor(&self) {
        let Some(window) = &self.window else {
            return;
        };
        if self.input_mode.is_playing() {
            if let Err(locked) = window.set_cursor_grab(CursorGrabMode::Locked) {
                if let Err(confined) = window.set_cursor_grab(CursorGrabMode::Confined) {
                    eprintln!(
                        "warning: cannot capture the mouse cursor (locking: {locked}; confining: {confined})"
                    );
                }
            }
            window.set_cursor_visible(false);
        } else {
            if let Err(error) = window.set_cursor_grab(CursorGrabMode::None) {
                eprintln!("warning: cannot release the mouse cursor: {error}");
            }
            window.set_cursor_visible(true);
        }
    }

    /// Runs `change` on the input mode and, if the mode or the menu selection changed, brings
    /// the window in line: the cursor, the held keys (forgotten when the menu opens, since
    /// their releases go to the menu) and the overlay.
    fn change_input_mode(
        &mut self,
        event_loop: &ActiveEventLoop,
        change: impl FnOnce(&mut InputMode),
    ) {
        let before = self.input_mode;
        change(&mut self.input_mode);
        if self.input_mode == before {
            return;
        }
        let effects = input_mode::effects(before.mode(), self.input_mode.mode());
        if effects.release_controls {
            self.controls.release_all();
        }
        if effects.apply_cursor {
            self.apply_cursor();
        }
        self.refresh_after_toggle(event_loop);
    }

    /// Uses a menu button: the mode change it asks for, or ending the game.
    fn use_menu_button(&mut self, event_loop: &ActiveEventLoop, button: Option<MenuButton>) {
        let mut outcome = Outcome::Continue;
        self.change_input_mode(event_loop, |input| {
            outcome = match button {
                Some(button) => input.activate(button),
                None => input.activate_selected(),
            };
        });
        if outcome == Outcome::Quit {
            event_loop.exit();
        }
    }

    /// The figures of the whole run, for the line printed when the game ends, the triangles
    /// the last frame drew per tile level and what streaming held at the end (decision 0015
    /// budgets them).
    fn run_summary(&self) -> Option<String> {
        let summary = self.run_frames.summary()?;
        let stream = self
            .streamer
            .as_ref()
            .map(ChunkStreamer::stats)
            .unwrap_or_default();
        Some(format!(
            "Run summary (frames: {}, play time: {:.1?}): {summary} | last frame triangles by level {:?} | horizon {} blocks, {} columns and {} tiles loaded, {:.1} MiB of meshes | {}",
            summary.frames,
            summary.covered,
            self.render_stats.triangles_per_level,
            self.horizon,
            stream.loaded,
            stream.tiles_loaded,
            stream.mesh_bytes as f64 / BYTES_PER_MIB,
            self.fill.waiting_summary()
        ))
    }

    /// Handles a key while the menu is open: Escape closes it, Up/W and Down/S move the
    /// selection, Enter uses the selected button. Everything else is ignored.
    /// The selection keys repeat when held; Escape and Enter act on the press only.
    fn handle_menu_key(&mut self, event_loop: &ActiveEventLoop, code: KeyCode, repeat: bool) {
        match code {
            KeyCode::Escape if !repeat => self.change_input_mode(event_loop, InputMode::escape),
            KeyCode::ArrowUp | KeyCode::KeyW => {
                self.change_input_mode(event_loop, InputMode::select_previous);
            }
            KeyCode::ArrowDown | KeyCode::KeyS => {
                self.change_input_mode(event_loop, InputMode::select_next);
            }
            KeyCode::Enter | KeyCode::NumpadEnter if !repeat => {
                self.use_menu_button(event_loop, None);
            }
            _ => {}
        }
    }

    /// Handles a keyboard event: Escape opens the menu, L toggles the frame limit, F3 shows or
    /// hides the metrics panel, and the movement keys go to the controls. The toggles act on
    /// the press only, not on the repeats of a held key. While the menu is open the menu gets
    /// the presses and releases are ignored (the controls were cleared when it opened).
    fn handle_key(
        &mut self,
        event_loop: &ActiveEventLoop,
        code: KeyCode,
        state: ElementState,
        repeat: bool,
    ) {
        let pressed = state == ElementState::Pressed;
        if self.input_mode.is_paused() {
            if pressed {
                self.handle_menu_key(event_loop, code, repeat);
            }
            return;
        }
        match code {
            KeyCode::Escape if pressed && !repeat => {
                self.change_input_mode(event_loop, InputMode::escape);
            }
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
        let space = Space {
            world: &self.world,
            spans: &self.spans,
            liquid: self.liquid,
        };
        if body.is_free(space) {
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
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x as f32, position.y as f32);
                // The mouse selects the button it is over, so Enter then uses that one too.
                if let Some(button) = self.button_under_cursor() {
                    self.change_input_mode(event_loop, |input| input.select(button));
                }
            }
            // The right button has no use yet.
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.input_mode.is_paused() {
                    // A click outside the buttons does nothing, and neither does the one that
                    // focused the window.
                    if input_mode::click_activates(self.controls.just_focused()) {
                        if let Some(button) = self.button_under_cursor() {
                            self.use_menu_button(event_loop, Some(button));
                        }
                    }
                } else {
                    self.controls.click();
                }
            }
            WindowEvent::MouseWheel { .. } if self.input_mode.is_paused() => {}
            WindowEvent::MouseWheel { delta, .. } => match delta {
                MouseScrollDelta::LineDelta(_, lines) => self.controls.scroll(lines),
                MouseScrollDelta::PixelDelta(pixels) => self.controls.scroll_pixels(pixels.y),
            },
            // Held keys would stay "held" forever, because their release goes to another window.
            // The menu opens too, which frees the cursor.
            WindowEvent::Focused(false) => {
                self.controls.release_all();
                self.change_input_mode(event_loop, InputMode::focus_lost);
            }
            WindowEvent::Focused(true) => {
                self.controls.focus_gained();
                self.change_input_mode(event_loop, InputMode::focus_gained);
                // Whatever the mode, the grab is set again: winit does not release it when
                // the focus is lost and only refreshes it while focused, so a paused window
                // must free the cursor the moment it is active again.
                self.apply_cursor();
            }
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
            if self.input_mode.is_playing() {
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
