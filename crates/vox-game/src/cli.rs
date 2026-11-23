//! Command-line options of the game.

/// The radius of the streamed circle of chunks when `--view-radius` is not given.
pub const DEFAULT_VIEW_RADIUS: i32 = 8;

/// The largest accepted `--view-radius`: a circle of about 2800 chunk columns around the camera
/// (about 3000 kept loaded with the unload margin), enough to measure how the frame time grows
/// with the chunk count while staying under the 4096 memory allocations Vulkan guarantees.
pub const MAX_VIEW_RADIUS: i32 = 30;

/// How fast the `--fly` autopilot moves the camera, in blocks per second: 5 chunks per
/// second, several times the player's speed, so streaming is stressed.
pub const FLY_SPEED: f32 = 160.0;

/// How the game was asked to run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    /// Open the window without taking the keyboard focus, so it does not interrupt
    /// whatever the player was doing in another window. Used by scripted checks.
    pub background: bool,
    /// Radius in chunks of the circle streamed in around the camera, 0..=[`MAX_VIEW_RADIUS`].
    pub view_radius: i32,
    /// Fly sideways at [`FLY_SPEED`] for this many seconds and then quit, for measuring
    /// how streaming behaves while the camera moves. `None` leaves the camera to the player.
    pub fly_seconds: Option<f32>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            background: false,
            view_radius: DEFAULT_VIEW_RADIUS,
            fly_seconds: None,
        }
    }
}

/// What the command line asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    /// Run the game with these options.
    Run(Options),
    /// Print [`USAGE`] and exit.
    Help,
}

/// The text printed for `--help` and after a mistake. The radius limits are written from the
/// constants, so the text cannot disagree with what the parser accepts.
pub fn usage() -> String {
    format!(
        "Usage: vox-game [--background] [--view-radius N] [--fly SECONDS]

Options:
  --background     open the window without taking the keyboard focus
  --view-radius N  chunks to stream in around the camera (a circle), 0 to {MAX_VIEW_RADIUS} (default {DEFAULT_VIEW_RADIUS})
  --fly SECONDS    fly sideways automatically, then quit (for measuring)
  -h, --help       print this text"
    )
}

/// Reads the arguments that follow the program name.
///
/// # Errors
/// Returns a message naming the first argument that is not recognized.
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let mut options = Options::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--background" => options.background = true,
            "--fly" => {
                let value = args.next().ok_or("--fly needs a number of seconds")?;
                options.fly_seconds = Some(parse_fly_seconds(&value)?);
            }
            "--view-radius" => {
                let value = args.next().ok_or("--view-radius needs a number")?;
                options.view_radius = parse_view_radius(&value)?;
            }
            "-h" | "--help" => return Ok(Command::Help),
            _ => return Err(format!("unknown option: {arg}")),
        }
    }
    Ok(Command::Run(options))
}

fn parse_fly_seconds(value: &str) -> Result<f32, String> {
    match value.parse::<f32>() {
        Ok(seconds) if seconds.is_finite() && seconds > 0.0 => Ok(seconds),
        _ => Err(format!(
            "--fly must be a positive number of seconds, got: {value}"
        )),
    }
}

fn parse_view_radius(value: &str) -> Result<i32, String> {
    match value.parse::<i32>() {
        Ok(radius) if (0..=MAX_VIEW_RADIUS).contains(&radius) => Ok(radius),
        _ => Err(format!(
            "--view-radius must be a whole number from 0 to {MAX_VIEW_RADIUS}, got: {value}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(args: &[&str]) -> Result<Command, String> {
        parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn no_arguments_run_the_game_in_the_foreground() {
        assert_eq!(parse_all(&[]), Ok(Command::Run(Options::default())));
    }

    #[test]
    fn background_is_recognized() {
        assert_eq!(
            parse_all(&["--background"]),
            Ok(Command::Run(Options {
                background: true,
                ..Options::default()
            }))
        );
    }

    #[test]
    fn help_wins_in_either_form() {
        assert_eq!(parse_all(&["--help"]), Ok(Command::Help));
        assert_eq!(parse_all(&["-h"]), Ok(Command::Help));
        assert_eq!(parse_all(&["--background", "--help"]), Ok(Command::Help));
    }

    #[test]
    fn an_unknown_option_is_named_in_the_error() {
        let error = parse_all(&["--background", "--fullscreen"]).unwrap_err();
        assert!(error.contains("--fullscreen"), "{error}");
    }

    #[test]
    fn view_radius_is_read_and_checked() {
        let run = |args: &[&str]| match parse_all(args) {
            Ok(Command::Run(options)) => Ok(options.view_radius),
            other => Err(format!("{other:?}")),
        };
        assert_eq!(run(&["--view-radius", "30"]), Ok(30));
        assert_eq!(run(&["--view-radius", "0"]), Ok(0));
        assert_eq!(run(&[]), Ok(DEFAULT_VIEW_RADIUS));
        assert!(run(&["--view-radius", "31"]).is_err());
        assert!(run(&["--view-radius", "-1"]).is_err());
        assert!(run(&["--view-radius", "x"]).is_err());
        assert!(run(&["--view-radius"]).is_err());
    }

    #[test]
    fn fly_takes_positive_seconds() {
        let fly = |args: &[&str]| match parse_all(args) {
            Ok(Command::Run(options)) => Ok(options.fly_seconds),
            other => Err(format!("{other:?}")),
        };
        assert_eq!(fly(&["--fly", "2.5"]), Ok(Some(2.5)));
        assert_eq!(fly(&[]), Ok(None));
        assert!(fly(&["--fly", "0"]).is_err());
        assert!(fly(&["--fly", "-1"]).is_err());
        assert!(fly(&["--fly", "NaN"]).is_err());
        assert!(fly(&["--fly"]).is_err());
    }

    #[test]
    fn the_usage_states_the_radius_limits() {
        let text = usage();
        assert!(text.contains(&MAX_VIEW_RADIUS.to_string()));
        assert!(text.contains(&DEFAULT_VIEW_RADIUS.to_string()));
    }

    #[test]
    fn the_usage_mentions_every_option() {
        let text = usage();
        assert!(text.contains("--background"));
        assert!(text.contains("--view-radius"));
        assert!(text.contains("--help"));
    }
}
