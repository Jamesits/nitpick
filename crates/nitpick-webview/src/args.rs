//! Command-line arguments and the page URL.

use clap::Parser;
use tauri::Url;

/// Query parameter that tells the page it runs in this shell.
const WEBVIEW_PARAMETER: &str = "webview";

/// Opens the nitpick browser page full screen on one local monitor.
#[derive(Debug, Parser)]
#[command(version)]
pub struct Args {
    /// URL of the nitpick browser service, `http` or `https`.
    #[arg(long, value_parser = page_url, required_unless_present = "list_monitors")]
    pub url: Option<Url>,
    /// A point on the target monitor, `<x>,<y>`, in the coordinates that
    /// `--list-monitors` prints.
    // `allow_negative_numbers` does not cover `-1920,540`: clap accepts only
    // a value that parses as one number.
    #[arg(
        long,
        value_parser = point,
        allow_hyphen_values = true,
        required_unless_present = "list_monitors"
    )]
    pub at: Option<(i64, i64)>,
    /// Print the monitors and exit.
    #[arg(long, conflicts_with_all = ["url", "at"])]
    pub list_monitors: bool,
}

/// Parses `<x>,<y>`.
pub fn point(text: &str) -> Result<(i64, i64), String> {
    let (x, y) = text
        .split_once(',')
        .ok_or_else(|| format!("expected <x>,<y>, got {text:?}"))?;
    let coordinate = |v: &str| {
        v.trim()
            .parse::<i32>()
            .map(i64::from)
            .map_err(|e| format!("invalid coordinate {v:?}: {e}"))
    };
    Ok((coordinate(x)?, coordinate(y)?))
}

/// Parses the service URL and adds the `webview` query parameter. The
/// page reads only the presence of the parameter.
pub fn page_url(text: &str) -> Result<Url, String> {
    let mut url = Url::parse(text).map_err(|e| format!("invalid URL {text:?}: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("expected an http or https URL, got {text:?}"));
    }
    if url.host().is_none() {
        return Err(format!("the URL has no host: {text:?}"));
    }
    if !url.query_pairs().any(|(k, _)| k == WEBVIEW_PARAMETER) {
        url.query_pairs_mut().append_key_only(WEBVIEW_PARAMETER);
    }
    Ok(url)
}

/// A navigation is allowed only inside the origin of the page, so that
/// the full-screen window cannot show another site.
pub fn allowed_navigation(page: &Url, target: &Url) -> bool {
    page.origin() == target.origin()
}

#[cfg(test)]
#[path = "args_tests.rs"]
mod tests;
