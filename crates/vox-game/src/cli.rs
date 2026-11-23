//! Command-line options of the game.

/// How the game was asked to run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// Open the window without taking the keyboard focus, so it does not interrupt
    /// whatever the player was doing in another window. Used by scripted checks.
    pub background: bool,
}

/// What the command line asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Run the game with these options.
    Run(Options),
    /// Print [`USAGE`] and exit.
    Help,
}

/// The text printed for `--help` and after a mistake.
pub const USAGE: &str = "Usage: vox-game [--background]

Options:
  --background  open the window without taking the keyboard focus
  -h, --help    print this text";

/// Reads the arguments that follow the program name.
///
/// # Errors
/// Returns a message naming the first argument that is not recognized.
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let mut options = Options::default();
    for arg in args {
        match arg.as_str() {
            "--background" => options.background = true,
            "-h" | "--help" => return Ok(Command::Help),
            _ => return Err(format!("unknown option: {arg}")),
        }
    }
    Ok(Command::Run(options))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(args: &[&str]) -> Result<Command, String> {
        parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn no_arguments_run_the_game_in_the_foreground() {
        assert_eq!(
            parse_all(&[]),
            Ok(Command::Run(Options { background: false }))
        );
    }

    #[test]
    fn background_is_recognized() {
        assert_eq!(
            parse_all(&["--background"]),
            Ok(Command::Run(Options { background: true }))
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
    fn the_usage_mentions_every_option() {
        assert!(USAGE.contains("--background"));
        assert!(USAGE.contains("--help"));
    }
}
