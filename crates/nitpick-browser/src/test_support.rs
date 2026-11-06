//! Test data shared by the unit tests of this crate.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use serde_json::{Map, Value, json};

use crate::client::Hello;

/// TEST-NET-1 (RFC 5737).
pub(crate) const PEER: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)), 50_000);

pub(crate) const SAFARI_IPAD: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
    AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";
pub(crate) const CHROME_WINDOWS: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36";
pub(crate) const EDGE_WINDOWS: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36 Edg/142.0.0.0";
pub(crate) const CHROME_LINUX: &str = "Mozilla/5.0 (X11; Linux x86_64) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36";
pub(crate) const FIREFOX_WINDOWS: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:145.0) Gecko/20100101 Firefox/145.0";
pub(crate) const FIREFOX_LINUX: &str =
    "Mozilla/5.0 (X11; Linux x86_64; rv:145.0) Gecko/20100101 Firefox/145.0";
pub(crate) const FIREFOX_MACOS: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 15.6; rv:145.0) Gecko/20100101 Firefox/145.0";
pub(crate) const FIREFOX_ANDROID: &str =
    "Mozilla/5.0 (Android 15; Mobile; rv:145.0) Gecko/145.0 Firefox/145.0";
pub(crate) const WEBKITGTK: &str = "Mozilla/5.0 (X11; Ubuntu; Linux x86_64) \
    AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15";

/// iPad Safari in full screen: 1180×820 CSS px at pixel ratio 2.
pub(crate) fn hello_json() -> Value {
    json!({
        "type": "hello",
        "client": {"userAgent": SAFARI_IPAD, "webview": false},
        "screen": {"width": 1180, "height": 820},
        "viewport": {"width": 1180, "height": 820, "scale": 1},
        "pixelRatio": 2,
        "dynamicRange": "high",
        "colorGamut": "p3",
        "fullscreen": true
    })
}

/// [`hello_json`] with `changes` applied; `None` removes the field.
pub(crate) fn hello_with(changes: &[(&str, Option<Value>)]) -> Value {
    let mut value = hello_json();
    for (path, change) in changes {
        let mut parts: Vec<&str> = path.split('.').collect();
        let last = parts.pop().expect("non-empty path");
        let mut object = &mut value;
        for part in parts {
            object = &mut object[part];
        }
        let object = object.as_object_mut().expect("object");
        match change {
            Some(v) => {
                object.insert(last.into(), v.clone());
            }
            None => {
                object.remove(last);
            }
        }
    }
    value
}

/// Fields as the session passes them to [`Hello::parse`].
pub(crate) fn fields(value: &Value) -> Map<String, Value> {
    let mut map = value.as_object().expect("object").clone();
    map.remove("type");
    map
}

pub(crate) fn hello(changes: &[(&str, Option<Value>)]) -> Hello {
    Hello::parse(fields(&hello_with(changes))).expect("valid hello")
}
