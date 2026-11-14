use super::*;

#[test]
fn point_parses_signed_coordinates() {
    assert_eq!(point("-1920,0"), Ok((-1920, 0)));
    assert_eq!(point("10, -20"), Ok((10, -20)));
}

#[test]
fn point_rejects_other_forms() {
    for text in [
        "",
        "10",
        "10;20",
        "1.5,2",
        "a,b",
        "10,20,30",
        "4294967296,0",
    ] {
        assert!(point(text).is_err(), "{text:?}");
    }
}

#[test]
fn page_url_adds_the_webview_parameter() {
    let url = page_url("http://127.0.0.1:8080/").unwrap();
    assert_eq!(url.as_str(), "http://127.0.0.1:8080/?webview");
    let url = page_url("https://[::1]:8080/?a=1").unwrap();
    assert_eq!(url.as_str(), "https://[::1]:8080/?a=1&webview");
}

#[test]
fn page_url_keeps_an_existing_webview_parameter() {
    let url = page_url("http://127.0.0.1:8080/?webview=1").unwrap();
    assert_eq!(url.as_str(), "http://127.0.0.1:8080/?webview=1");
}

#[test]
fn page_url_rejects_other_schemes_and_relative_urls() {
    for text in [
        "file:///C:/index.html",
        "ws://127.0.0.1/",
        "127.0.0.1:8080",
        "/index.html",
    ] {
        assert!(page_url(text).is_err(), "{text:?}");
    }
}

#[test]
fn navigation_stays_in_the_page_origin() {
    let page = page_url("http://127.0.0.1:8080/").unwrap();
    let to = |s: &str| Url::parse(s).unwrap();
    assert!(allowed_navigation(&page, &to("http://127.0.0.1:8080/")));
    assert!(allowed_navigation(&page, &to("http://127.0.0.1:8080/x?y")));
    assert!(!allowed_navigation(&page, &to("http://127.0.0.1:8081/")));
    assert!(!allowed_navigation(&page, &to("https://127.0.0.1:8080/")));
    assert!(!allowed_navigation(&page, &to("http://localhost:8080/")));
    assert!(!allowed_navigation(&page, &to("about:blank")));
}

#[test]
fn list_monitors_excludes_the_window_arguments() {
    assert!(Args::try_parse_from(["nitpick-webview", "--list-monitors"]).is_ok());
    assert!(Args::try_parse_from(["nitpick-webview", "--url", "http://127.0.0.1/"]).is_err());
    assert!(Args::try_parse_from(["nitpick-webview", "--list-monitors", "--at", "0,0"]).is_err());
    let args = Args::try_parse_from([
        "nitpick-webview",
        "--url",
        "http://127.0.0.1/",
        "--at",
        "0,0",
    ])
    .unwrap();
    assert_eq!(args.at, Some((0, 0)));
}
