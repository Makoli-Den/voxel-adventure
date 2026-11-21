//! Game entry point.
//!
//! This is where the layers are wired together: it opens the window, creates the
//! renderer and runs the event loop. World, mods and networking will join later.

use std::error::Error;
use std::fmt::Display;
use std::process::ExitCode;
use std::sync::Arc;

use vox_render::{Renderer, create_renderer};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

/// Application state driven by the window event loop.
#[derive(Default)]
struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Box<dyn Renderer>>,
    /// True while the window has zero size. The loop sleeps instead of spinning.
    minimized: bool,
    /// The error that stopped the game, reported after the event loop ends.
    error: Option<String>,
}

impl App {
    /// Opens the window and creates the renderer for it.
    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<(), Box<dyn Error>> {
        let attributes = Window::default_attributes().with_title("Voxel Adventure");
        let window = Arc::new(event_loop.create_window(attributes)?);
        let size = window.inner_size();
        let renderer = create_renderer(Arc::clone(&window), size.width, size.height)?;
        println!("Rendering on: {}", renderer.device_name());

        self.window = Some(window);
        self.renderer = Some(renderer);
        Ok(())
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
            WindowEvent::Resized(size) => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.resize(size.width, size.height);
                }
                // Draw continuously, but sleep until the next event while minimized.
                self.minimized = size.width == 0 || size.height == 0;
                event_loop.set_control_flow(if self.minimized {
                    ControlFlow::Wait
                } else {
                    ControlFlow::Poll
                });
            }
            WindowEvent::RedrawRequested => {
                let result = self.renderer.as_mut().map(|r| r.render_frame());
                if let Some(Err(error)) = result {
                    self.fail(event_loop, error);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Ask for the next frame right away: the game redraws continuously.
        if self.minimized {
            return;
        }
        if let Some(window) = &self.window {
            window.request_redraw();
        }
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

    let mut app = App::default();
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
