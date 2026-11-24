//! Command-line options of the game.

use std::path::PathBuf;

/// The radius of the streamed circle of chunks when `--view-radius` is not given: the largest
/// of 8, 12 and 16 that kept at least 2000 FPS standing and 1000 FPS flying (`--fly 8`) on the
/// development machine with fog (decision 0015 S1, `docs/benchmarks.md`); 16 reached about 1700
/// standing.
pub const DEFAULT_VIEW_RADIUS: i32 = 12;

/// The largest accepted `--view-radius`: a circle of about 2800 chunk columns around the camera
/// (about 3000 kept loaded with the unload margin), enough to measure how the frame time grows
/// with the chunk count while staying under the 4096 memory allocations Vulkan guarantees.
pub const MAX_VIEW_RADIUS: i32 = 30;

/// How fast the `--fly` autopilot moves the camera, in blocks per second: 5 chunks per
/// second, several times the player's speed, so streaming is stressed.
pub const FLY_SPEED: f32 = 160.0;

/// How the game was asked to run.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// Open the window without taking the keyboard focus, so it does not interrupt
    /// whatever the player was doing in another window. Used by scripted checks.
    pub background: bool,
    /// Radius in chunks of the circle streamed in around the camera, 0..=[`MAX_VIEW_RADIUS`].
    pub view_radius: i32,
    /// Fly sideways at [`FLY_SPEED`] for this many seconds and then quit, for measuring
    /// how streaming behaves while the camera moves. `None` leaves the camera to the player.
    pub fly_seconds: Option<f32>,
    /// Resource pack directories from `--pack`, in command-line order. They are layered over
    /// the built-in pack, each one above the previous, so a later pack overrides an earlier
    /// one file by file (decision 0013).
    pub packs: Vec<PathBuf>,
    /// Mods folders from `--mods`, in command-line order. Each holds one folder per mod
    /// (decision 0014). Empty means the built-in mods folder, so the base game loads.
    pub mods: Vec<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            background: false,
            view_radius: DEFAULT_VIEW_RADIUS,
            fly_seconds: None,
            packs: Vec::new(),
            mods: Vec::new(),
        }
    }
}

/// What the command line asks for.
#[derive(Clone, Debug, PartialEq)]
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
        "Usage: vox-game [--background] [--view-radius N] [--fly SECONDS] [--pack DIR]... [--mods DIR]...

Options:
  --background     open the window without taking the keyboard focus
  --view-radius N  chunks to stream in around the camera (a circle), 0 to {MAX_VIEW_RADIUS} (default {DEFAULT_VIEW_RADIUS})
  --fly SECONDS    fly sideways automatically, then quit (for measuring)
  --pack DIR       use the resource pack in DIR over the built-in one; repeatable, a later
                   pack overrides an earlier one
  --mods DIR       load the mods found in DIR instead of the built-in mods folder; repeatable,
                   a mod id may appear in only one folder; without the base mod (`base`)
                   in one of them the game stops
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
            "--pack" => {
                let dir = args.next().ok_or("--pack needs a directory")?;
                options.packs.push(parse_dir("--pack", &dir)?);
            }
            "--mods" => {
                let dir = args.next().ok_or("--mods needs a directory")?;
                options.mods.push(parse_dir("--mods", &dir)?);
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

/// Rejects an empty directory and one that looks like an option: `--pack --background` is
/// almost surely a forgotten directory, not a folder named `--background`. `option` names the
/// option the value belongs to, for the message.
fn parse_dir(option: &str, value: &str) -> Result<PathBuf, String> {
    if value.is_empty() || value.starts_with("--") {
        return Err(format!("{option} needs a directory, got: {value:?}"));
    }
    Ok(PathBuf::from(value))
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
        assert!(text.contains("--pack"));
        assert!(text.contains("--mods"));
        assert!(text.contains("--help"));
    }

    #[test]
    fn packs_are_kept_in_command_line_order() {
        let packs = |args: &[&str]| match parse_all(args) {
            Ok(Command::Run(options)) => Ok(options.packs),
            other => Err(format!("{other:?}")),
        };
        assert_eq!(packs(&[]), Ok(Vec::new()));
        assert_eq!(
            packs(&["--pack", "first", "--background", "--pack", "second"]),
            Ok(vec![PathBuf::from("first"), PathBuf::from("second")])
        );
        assert_eq!(
            packs(&["--pack", "same", "--pack", "same"]),
            Ok(vec![PathBuf::from("same"), PathBuf::from("same")])
        );
    }

    #[test]
    fn pack_needs_a_directory() {
        let error = parse_all(&["--pack"]).unwrap_err();
        assert!(error.contains("--pack"), "{error}");
    }

    #[test]
    fn pack_rejects_an_empty_value_and_an_option() {
        let error = parse_all(&["--pack", ""]).unwrap_err();
        assert!(error.contains("--pack needs a directory"), "{error}");
        let error = parse_all(&["--pack", "--background"]).unwrap_err();
        assert!(error.contains("--background"), "{error}");
    }

    #[test]
    fn mods_folders_are_kept_in_command_line_order() {
        let mods = |args: &[&str]| match parse_all(args) {
            Ok(Command::Run(options)) => Ok(options.mods),
            other => Err(format!("{other:?}")),
        };
        assert_eq!(mods(&[]), Ok(Vec::new()));
        assert_eq!(
            mods(&["--mods", "first", "--background", "--mods", "second"]),
            Ok(vec![PathBuf::from("first"), PathBuf::from("second")])
        );
    }

    #[test]
    fn mods_needs_a_directory() {
        let error = parse_all(&["--mods"]).unwrap_err();
        assert!(error.contains("--mods needs a directory"), "{error}");
    }

    #[test]
    fn mods_rejects_an_empty_value_and_an_option() {
        let error = parse_all(&["--mods", ""]).unwrap_err();
        assert!(error.contains("--mods needs a directory"), "{error}");
        let error = parse_all(&["--mods", "--background"]).unwrap_err();
        assert!(error.contains("--mods needs a directory"), "{error}");
        assert!(error.contains("--background"), "{error}");
    }
}
