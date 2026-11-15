use nitpick_core::driver::{DisplayId, OutputError};

use super::*;

fn id() -> DisplayId {
    DisplayId {
        driver: "browser",
        key: "client-1".into(),
    }
}

#[test]
fn maps_each_service_event() {
    let display = |state| {
        DriverEvent::Notice(Event::Display {
            state,
            display: None,
            reason: None,
        })
    };
    let cases = [
        (
            ServiceEvent::Listening {
                url: "http://127.0.0.1:1/".into(),
            },
            DriverEvent::Notice(Event::Listening {
                url: "http://127.0.0.1:1/".into(),
            }),
        ),
        (
            ServiceEvent::PairingCode("0042".into()),
            DriverEvent::Notice(Event::PairingCode {
                code: "0042".into(),
            }),
        ),
        (
            ServiceEvent::Client(ClientState::Connected),
            display(DisplayState::Connected),
        ),
        (
            ServiceEvent::Client(ClientState::Authenticated),
            display(DisplayState::Authenticated),
        ),
        (
            ServiceEvent::Client(ClientState::Ready(id())),
            DriverEvent::Ready(id()),
        ),
        (
            ServiceEvent::Client(ClientState::Disconnected("gone".into())),
            DriverEvent::Disconnected {
                display: None,
                reason: "gone".into(),
            },
        ),
        (
            ServiceEvent::Stopped {
                error: Some("e".into()),
            },
            DriverEvent::Stopped {
                error: Some("e".into()),
            },
        ),
    ];
    for (event, want) in cases {
        assert_eq!(driver_event(event), want);
    }
}

/// The pending frame gets the error of the driver for the same change.
#[test]
fn a_viewport_change_carries_the_driver_error() {
    let state = ClientState::ViewportChanged(id());
    let DriverEvent::Changed { display, error } = driver_event(ServiceEvent::Client(state.clone()))
    else {
        panic!("expected a change");
    };
    assert_eq!(display, id());
    assert_eq!(Some(error), state.presentation_error());
    assert!(matches!(
        state.presentation_error(),
        Some(OutputError::Platform(_))
    ));
}
