//! Console commands: one command per stdin line, words separated by
//! white space.

use nitpick_core::pattern::PatternKind;
use nitpick_core::signal::{BitDepth, Encoding};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    /// List the clients with a valid `hello`.
    Displays,
    /// Select a client by its `DisplayId` text, for example
    /// `browser:client-1`.
    Select(String),
    Pattern(PatternKind),
    Format(Encoding, BitDepth),
    /// Measurement delay of the next frames, in ms.
    Delay(u32),
    /// Query, open, render and present with the current settings.
    Show,
    /// Print the reference table for the selected client.
    Table,
    /// Cancel the pending frame and close the surface.
    Close,
    Quit,
}

pub const HELP: &str = "commands:
  displays                     list connected clients
  select <display id>          select a client, for example browser:client-1
  pattern <id>                 bt2111 or pq-grayscale
  format <encoding> <depth>    encoding pq-narrow, pq-full or hlg-narrow; depth 10 or 12
  delay <ms>                   measurement delay after the acknowledgement, >= 0 ms
  show                         show the pattern on the selected client
  table                        print the reference table of the selected client
  close                        cancel the pending frame and close the surface
  quit                         close the surface and stop the service";

/// `Ok(None)` for an empty line.
///
/// # Errors
/// An unknown command, or a missing, extra or invalid argument.
pub fn parse(line: &str) -> Result<Option<Command>, String> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let Some((&name, args)) = words.split_first() else {
        return Ok(None);
    };
    let command = match (name, args) {
        ("help", []) => Command::Help,
        ("displays", []) => Command::Displays,
        ("select", [id]) => Command::Select((*id).into()),
        ("pattern", [id]) => Command::Pattern(id.parse()?),
        ("format", [encoding, depth]) => Command::Format(encoding.parse()?, depth.parse()?),
        ("delay", [ms]) => Command::Delay(parse_delay(ms)?),
        ("show", []) => Command::Show,
        ("table", []) => Command::Table,
        ("close", []) => Command::Close,
        ("quit", []) => Command::Quit,
        (
            "help" | "displays" | "select" | "pattern" | "format" | "delay" | "show" | "table"
            | "close" | "quit",
            _,
        ) => return Err(format!("wrong arguments for `{name}`; type `help`")),
        _ => return Err(format!("unknown command `{name}`; type `help`")),
    };
    Ok(Some(command))
}

/// Measurement delay of the `--delay` option and the `delay` command.
///
/// # Errors
/// `text` is not an integer in `0..=u32::MAX`.
pub fn parse_delay(text: &str) -> Result<u32, String> {
    text.parse()
        .map_err(|_| format!("the measurement delay must be an integer >= 0 ms, not `{text}`"))
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
