use super::{arguments, describe_exit};

fn lines(text: &[&str]) -> Vec<String> {
    text.iter().map(|&l| l.to_owned()).collect()
}

#[test]
fn the_exit_names_the_first_error_line() {
    // `nitpick-webview` prints the monitor list after the error.
    let stderr = lines(&[
        "error: no monitor contains the position 9999,9999",
        "  \\\\.\\DISPLAY1\t0,0 3840x2160",
    ]);
    assert_eq!(
        describe_exit("exit status 1", &stderr),
        "exit status 1: error: no monitor contains the position 9999,9999"
    );
}

#[test]
fn without_an_error_line_the_exit_names_the_last_line() {
    let stderr = lines(&["warning: WebKitGTK shows HDR images as SDR", "  ", ""]);
    assert_eq!(
        describe_exit("exit status 0", &stderr),
        "exit status 0: warning: WebKitGTK shows HDR images as SDR"
    );
    assert_eq!(describe_exit("exit status 0", &[]), "exit status 0");
}

#[test]
fn a_negative_point_stays_in_its_flag_argument() {
    assert_eq!(
        arguments("http://127.0.0.1:8080/", (-1920, 540)),
        ["--url=http://127.0.0.1:8080/", "--at=-1920,540"]
    );
}
