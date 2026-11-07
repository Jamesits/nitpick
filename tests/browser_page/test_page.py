"""Client page: authentication, reconnect and `hello` reports in a real browser.

The page runs from the real service in a normal browser, without Tauri. Chromium
DevTools calls change the pixel ratio, pinch zoom and media features at run time.
"""

import json

import pytest

from conftest import CODE, REJECTED, TIMEOUT_S, VIEWPORT, Client, launch, ready_client


@pytest.mark.parametrize(
    ("dpr", "viewport", "size", "exact"),
    [
        (1, VIEWPORT, [800, 600], "unknown"),
        (2, VIEWPORT, [1600, 1200], "unknown"),
        # Fractional ratio: 853 × 1.5 = 1279.5 rounds up in both page and server.
        (1.5, {"width": 853, "height": 480}, [1280, 720], "no"),
    ],
)
def test_authenticates_and_reports_state(client_factory, host, dpr, viewport, size, exact):
    c = client_factory(dpr=dpr, viewport=viewport)
    c.authenticate()
    hello = c.wait_hello(1)

    auth = c.sent[0]
    assert auth == {"type": "auth", "code": CODE}
    assert hello["client"] == {
        "userAgent": c.page.evaluate("navigator.userAgent"),
        "webview": False,
    }
    assert hello["viewport"] == {**viewport, "scale": 1}
    assert hello["pixelRatio"] == dpr
    assert hello["fullscreen"] is False
    assert hello["screen"]["width"] > 0 and hello["screen"]["height"] > 0
    assert hello["dynamicRange"] in ("high", "standard")
    assert hello["colorGamut"] in ("rec2020", "p3", "srgb")

    view = ready_client(host)
    assert view["size"] == size
    assert view["pixelRatio"] == dpr
    assert exact in view["pixelExact"]
    assert "not full screen" in view["description"]

    # The token stays in memory: not in storage, cookies or the URL.
    assert c.page.evaluate("localStorage.length + sessionStorage.length") == 0
    assert c.context.cookies() == []
    assert c.page.url == host.url
    assert c.page.locator("#auth").is_hidden()


def test_webview_parameter(client_factory, host):
    c = client_factory()
    c.page.goto(host.url + "?webview")
    c.authenticate()
    assert c.wait_hello(1)["client"]["webview"] is True
    assert " webview at 127.0.0.1" in ready_client(host)["name"]


def test_hdr_display_reports_high_range(playwright_instance, host, is_chromium):
    if not is_chromium:
        pytest.skip("the HDR colour profile switch is for Chromium")
    hdr = launch(playwright_instance, ["--force-color-profile=hdr10"])
    try:
        c = Client(hdr, host.url)
        c.authenticate()
        assert c.wait_hello(1)["dynamicRange"] == "high"
        assert ready_client(host)["hdrOn"] is True
    finally:
        hdr.close()


def test_wrong_code_locks_then_reconnects(client_factory, host):
    c = client_factory()
    c.authenticate("9999")
    c.wait_status("Wrong code.")
    host.wait("disconnected", reason="wrong code")
    assert c.page.locator("#connect").is_visible()
    assert c.hellos() == []

    # The first failure locks the service for 1 s; the upgrade gets 429.
    c.page.click("#connect")
    c.wait_status(REJECTED)
    c.page.wait_for_timeout(1100)
    c.page.click("#connect")
    c.authenticate()
    c.wait_hello(1)
    ready_client(host)


def test_reload_starts_a_new_session(client_factory, host):
    c = client_factory()
    c.authenticate()
    c.wait_hello(1)
    first = ready_client(host)["display"]

    c.page.reload()
    host.wait("disconnected")
    c.authenticate()
    c.wait_hello(2)
    second = ready_client(host)["display"]
    assert second != first


def test_second_client_is_rejected(client_factory, host):
    first = client_factory()
    first.authenticate()
    first.wait_hello(1)
    ready_client(host)

    second = client_factory()
    second.wait_status(REJECTED)
    assert second.page.locator("#auth").is_hidden()
    assert second.page.locator("#connect").is_visible()
    assert not [e for e in host.drain() if e["event"] in ("connected", "disconnected")]


@pytest.mark.parametrize(
    ("code", "reason", "text"),
    [
        (4002, "authentication timeout", "No code within 60 s. (authentication timeout)"),
        (4003, "expected auth", "Protocol error (expected auth)"),
        (1011, "no random data for the code", "Server error (no random data for the code)"),
    ],
)
def test_close_codes_before_authentication(browser, host, code, reason, text):
    # A routed WebSocket stands in for the server: the real one takes 60 s for
    # the timeout and cannot be made to fail the random generator.
    context = browser.new_context()
    try:
        page = context.new_page()

        routes = []

        def server(ws):
            ws.send(json.dumps({"type": "auth_required"}))
            routes.append(ws)

        page.route_web_socket("**/api/v1/ws", server)
        page.goto(host.url)
        page.locator("#status", has_text="Enter the code").wait_for(timeout=TIMEOUT_S * 1000)
        # Outside the handler: `close` in the sync handler waits for the
        # dispatcher that runs the handler.
        routes[0].close(code=code, reason=reason)
        page.locator("#status", has_text=text).wait_for(timeout=TIMEOUT_S * 1000)
        assert page.locator("#connect").is_visible()
        assert page.locator("#auth").is_hidden()
    finally:
        context.close()


def test_server_close_after_authentication(browser, host):
    context = browser.new_context()
    try:
        page = context.new_page()
        page.goto(host.url)
        page.locator("#status", has_text="Enter the code").wait_for()
        page.fill("#code", CODE)
        page.click("#submit")
        host.wait("ready")
        host.close()
        page.locator("#status", has_text="Disconnected").wait_for()
        assert page.locator("#connect").is_visible()
        assert page.locator("#hide").is_hidden()
    finally:
        context.close()


def test_browser_zoom_changes_pixel_ratio(client_factory, host, is_chromium):
    if not is_chromium:
        pytest.skip("needs Chromium DevTools")
    c = client_factory()
    c.authenticate()
    c.wait_hello(1)
    first = ready_client(host)

    # Browser zoom 125 %: the pixel ratio grows and the CSS viewport shrinks.
    c.cdp(
        "Emulation.setDeviceMetricsOverride",
        {"width": 640, "height": 480, "deviceScaleFactor": 1.25, "mobile": False},
    )
    hello = c.wait_hello(2)
    assert hello["pixelRatio"] == 1.25
    assert hello["viewport"] == {"width": 640, "height": 480, "scale": 1}
    host.wait("viewport_changed", display=first["display"])
    view = ready_client(host)
    assert view["size"] == [800, 600]
    assert "no" in view["pixelExact"]

    # A second change checks that the page watches the new ratio.
    c.cdp(
        "Emulation.setDeviceMetricsOverride",
        {"width": 400, "height": 300, "deviceScaleFactor": 2, "mobile": False},
    )
    hello = c.wait_hello(3)
    assert hello["pixelRatio"] == 2
    assert ready_client(host)["size"] == [800, 600]


def test_pinch_zoom_changes_scale(client_factory, host, is_chromium):
    if not is_chromium:
        pytest.skip("needs Chromium DevTools")
    c = client_factory(dpr=2)
    c.authenticate()
    c.wait_hello(1)
    ready_client(host)

    c.cdp("Emulation.setPageScaleFactor", {"pageScaleFactor": 2})
    hello = c.wait_hello(2)
    # The layout viewport and the surface keep their size.
    assert hello["viewport"] == {**VIEWPORT, "scale": 2}
    host.wait("viewport_changed")
    view = ready_client(host)
    assert view["size"] == [1600, 1200]
    assert "zoom 2" in view["pixelExact"]["no"]


def test_media_query_change(client_factory, host, is_chromium):
    if not is_chromium:
        pytest.skip("needs Chromium DevTools")
    c = client_factory()
    c.authenticate()
    assert c.wait_hello(1)["colorGamut"] == "srgb"
    ready_client(host)

    c.cdp(
        "Emulation.setEmulatedMedia",
        {"features": [{"name": "color-gamut", "value": "rec2020"}]},
    )
    assert c.wait_hello(2)["colorGamut"] == "rec2020"
    # The geometry did not change: the open surface stays valid.
    events = [e["event"] for e in host.drain(0.5)]
    assert "ready" in events and "viewport_changed" not in events


def test_fullscreen_button(client_factory, host):
    c = client_factory()
    c.authenticate()
    c.wait_hello(1)
    ready_client(host)
    assert c.page.locator("#fullscreen").is_visible()

    c.page.click("#fullscreen")
    hello = c.wait_hello(2)
    assert hello["fullscreen"] is True
    assert "not full screen" not in ready_client(host)["description"]
    # An authenticated full-screen page shows only the pattern.
    assert c.page.locator("#panel").is_hidden()

    # A click shows the panel; "Hide" hides it again.
    c.page.mouse.click(400, 300)
    assert c.page.locator("#panel").is_visible()
    assert c.page.locator("#fullscreen").is_hidden()
    c.page.click("#hide")
    assert c.page.locator("#panel").is_hidden()

    c.page.evaluate("document.exitFullscreen()")
    assert c.wait_hello(3)["fullscreen"] is False
    assert c.page.locator("#fullscreen").is_visible()


def test_unchanged_state_sends_no_hello(client_factory, host):
    c = client_factory()
    c.authenticate()
    c.wait_hello(1)
    ready_client(host)

    c.page.evaluate("window.dispatchEvent(new Event('resize'))")
    c.page.wait_for_timeout(300)
    assert len(c.hellos()) == 1
    assert not [e for e in host.drain() if e["event"] == "ready"]
