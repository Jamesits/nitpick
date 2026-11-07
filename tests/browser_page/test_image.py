"""Client page: frame fetch, decode, layout and acknowledgement in a real browser.

`page_host` presents real frames through `BrowserDriver`. Routes on the frame path
delay or replace responses. An init script records object URLs, decode calls,
DOM changes, animation-frame callbacks and the replies of the page, and can hold
a decode until the test releases it.
"""

import contextlib
import io
import struct
import time
import zlib

import pytest
from PIL import Image
from playwright.sync_api import Error as PlaywrightError

from conftest import TIMEOUT_S, VIEWPORT, ready_client

# PQ narrow range at 10 bits: black and nominal peak.
BLACK = [64, 64, 64]
PEAK = [940, 940, 940]
FRAME_PATH = "**/api/v1/frame/*"

PROBE = """
(() => {
  const log = [];
  const urls = new Set();
  window.__probe = { log, urls, gate: null, release: null };
  const create = URL.createObjectURL.bind(URL);
  const revoke = URL.revokeObjectURL.bind(URL);
  URL.createObjectURL = (blob) => {
    const url = create(blob);
    urls.add(url);
    log.push(["create", url]);
    return url;
  };
  URL.revokeObjectURL = (url) => {
    urls.delete(url);
    log.push(["revoke", url]);
    revoke(url);
  };
  const decode = HTMLImageElement.prototype.decode;
  HTMLImageElement.prototype.decode = async function () {
    const gate = window.__probe.gate;
    log.push(["decode", this.src]);
    try {
      await decode.call(this);
    } finally {
      if (gate) await gate;
    }
    log.push(["decoded", this.src]);
  };
  const raf = window.requestAnimationFrame.bind(window);
  window.requestAnimationFrame = (cb) => raf((t) => {
    log.push(["raf"]);
    cb(t);
  });
  const send = WebSocket.prototype.send;
  WebSocket.prototype.send = function (data) {
    const m = JSON.parse(data);
    if (m.type === "applied" || m.type === "error") log.push([m.type, m.id, m]);
    return send.call(this, data);
  };
  document.addEventListener("DOMContentLoaded", () => {
    new MutationObserver(() => log.push(["dom"])).observe(
      document.getElementById("stage"), { childList: true });
  });
})();
"""

# A function: `evaluate` of a promise expression waits for the promise.
HOLD_DECODE = """() => {
  window.__probe.gate = new Promise((resolve) => { window.__probe.release = resolve; });
}"""


def png(width, height):
    """An 8-bit RGB PNG of the given size."""

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    raw = b"".join(b"\0" + b"\0\0\0" * width for _ in range(height))
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw))
        + chunk(b"IEND", b"")
    )


@pytest.fixture
def session(client_factory, host):
    """An authenticated page with the probe and its first driver view."""

    def make(**kwargs):
        c = client_factory(init_script=PROBE, **kwargs)
        host.pump = lambda: c.page.wait_for_timeout(1)
        c.authenticate()
        c.wait_hello(1)
        return c, ready_client(host)

    return make


def show(host, seq, pattern=None, depth=10):
    host.send(cmd="show", seq=seq, depth=depth, pattern=pattern or {"fill": PEAK})


def replies(c):
    """`applied` and `error` messages in send order."""
    return probe(c, "p.log.filter((e) => e[0] === 'applied' || e[0] === 'error').map((e) => e[2])")


def probe(c, expr):
    return c.page.evaluate(f"(() => {{ const p = window.__probe; return {expr}; }})()")


def wait_page(c, expr, timeout=TIMEOUT_S):
    deadline = time.monotonic() + timeout
    while not probe(c, expr):
        assert time.monotonic() < deadline, f"no {expr}; log: {probe(c, 'p.log')}"
        c.page.wait_for_timeout(20)


def frames(c):
    """Properties of each `<img>` on the page."""
    return c.page.evaluate(
        """[...document.querySelectorAll("img")].map((img) => ({
          src: img.src,
          natural: [img.naturalWidth, img.naturalHeight],
          css: [img.getBoundingClientRect().width, img.getBoundingClientRect().height],
          style: (({ position, left, top, imageRendering, transform, opacity }) =>
            ({ position, left, top, imageRendering, transform, opacity }))(getComputedStyle(img)),
        }))"""
    )


def frame_requests(c):
    """Requests of the frame path, recorded from page creation."""
    requests = []
    c.page.on(
        "request",
        lambda r: "/api/v1/frame/" in r.url and requests.append((r.url, r.headers)),
    )
    return requests


@pytest.mark.parametrize(
    ("dpr", "viewport", "size", "exact"),
    [
        (1, VIEWPORT, [800, 600], True),
        (2, VIEWPORT, [1600, 1200], True),
        (1.5, {"width": 853, "height": 480}, [1280, 720], False),
    ],
)
def test_applies_frame_at_display_pixels(session, host, dpr, viewport, size, exact):
    c, _ = session(dpr=dpr, viewport=viewport)
    requests = frame_requests(c)
    show(host, 1)
    result = host.wait("presented", seq=1)

    [applied] = replies(c)
    assert applied["type"] == "applied"
    assert [applied["naturalWidth"], applied["naturalHeight"]] == size
    assert applied["layoutWidth"] == pytest.approx(size[0], abs=0.01)
    assert applied["layoutHeight"] == pytest.approx(size[1], abs=0.01)
    assert ("Pixel-exact layout" in result["status"]) == exact

    [img] = frames(c)
    assert img["natural"] == size
    assert img["css"] == [
        pytest.approx(size[0] / dpr, abs=0.01),
        pytest.approx(size[1] / dpr, abs=0.01),
    ]
    assert img["style"] == {
        "position": "fixed",
        "left": "0px",
        "top": "0px",
        "imageRendering": "pixelated",
        "transform": "none",
        "opacity": "1",
    }
    assert img["src"].startswith("blob:")
    assert probe(c, "[...p.urls]") == [img["src"]]

    # The token goes in the header only.
    [(url, headers)] = requests
    assert url == f"{host.url}api/v1/frame/{applied['id']}.png"
    assert headers["authorization"].startswith("Bearer ")
    assert "Bearer" not in url and "token" not in url


@pytest.mark.parametrize("dpr", [1, 2])
def test_one_image_pixel_per_display_pixel(session, host, dpr):
    c, _ = session(dpr=dpr)
    show(host, 1, {"columns": [BLACK, PEAK]})
    host.wait("presented", seq=1)
    c.page.click("#hide")

    shot = Image.open(io.BytesIO(c.page.screenshot(scale="device"))).convert("L")
    assert shot.size == (VIEWPORT["width"] * dpr, VIEWPORT["height"] * dpr)
    # A browser screenshot is SDR: it can show the column order, not the levels.
    row = [shot.getpixel((x, shot.height // 2)) for x in range(shot.width)]
    assert all(v < 64 for v in row[0::2]), row[:16]
    assert all(v > 128 for v in row[1::2]), row[:16]


def test_acknowledges_after_decode_dom_and_two_animation_frames(session, host):
    c, _ = session()
    show(host, 1)
    host.wait("presented", seq=1)
    assert [m["type"] for m in replies(c)] == ["applied"]

    log = [e[0] for e in probe(c, "p.log")]
    decoded = log.index("decoded")
    assert log[decoded:] == ["decoded", "dom", "raf", "raf", "applied"]


def test_replacement_releases_the_old_image(session, host):
    c, _ = session()
    show(host, 1)
    host.wait("presented", seq=1)
    [first] = frames(c)

    show(host, 2, {"fill": BLACK})
    host.wait("presented", seq=2)
    [second] = frames(c)
    assert second["src"] != first["src"]
    assert probe(c, "[...p.urls]") == [second["src"]]
    ids = [m["id"] for m in replies(c) if m["type"] == "applied"]
    assert len(ids) == 2 and ids[0] < ids[1]

    # `close` shows the black page and releases the last image.
    host.send(cmd="close")
    host.wait("closed")
    wait_page(c, "document.querySelectorAll('img').length === 0")
    assert probe(c, "p.urls.size") == 0


def test_http_error_fails_the_frame(session, host):
    c, _ = session()
    show(host, 1)
    host.wait("presented", seq=1)

    c.page.route(FRAME_PATH, lambda route: route.fulfill(status=500, body="no"))
    show(host, 2)
    failed = host.wait("present_failed", seq=2)
    assert "HTTP 500" in failed["error"]
    assert replies(c)[-1]["type"] == "error"
    # The failed frame does not leave the previous pattern on the page.
    assert frames(c) == []
    assert probe(c, "p.urls.size") == 0


def test_decode_error_fails_the_frame(session, host):
    c, _ = session()
    c.page.route(
        FRAME_PATH,
        lambda route: route.fulfill(status=200, content_type="image/png", body=b"not a png"),
    )
    show(host, 1)
    failed = host.wait("present_failed", seq=1)
    assert "decode failed" in failed["error"]
    assert frames(c) == []
    # The URL of the failed decode is released.
    log = probe(c, "p.log")
    [created] = [e[1] for e in log if e[0] == "create"]
    assert ["revoke", created] in log
    assert probe(c, "p.urls.size") == 0


def test_wrong_natural_size_fails_the_frame(session, host):
    c, _ = session()
    c.page.route(
        FRAME_PATH,
        lambda route: route.fulfill(status=200, content_type="image/png", body=png(3, 2)),
    )
    show(host, 1)
    failed = host.wait("present_failed", seq=1)
    assert "decoded frame" in failed["error"] and "3×2 px, not 800×600" in failed["error"]
    [applied] = replies(c)
    assert [applied["naturalWidth"], applied["naturalHeight"]] == [3, 2]


def hold_first_fetch(c):
    """Routes the frame path; the first response waits in the returned list."""
    held = []

    def handle(route):
        if held:
            route.continue_()
            return
        # The server keeps the latest PNG only: fetch it before the next
        # frame replaces it.
        held.append((route, route.fetch()))

    c.page.route(FRAME_PATH, handle)
    return held


def release(held):
    route, response = held[0]
    # The page can have aborted the request.
    with contextlib.suppress(PlaywrightError):
        route.fulfill(response=response)


def wait_held(c, held):
    deadline = time.monotonic() + TIMEOUT_S
    while not held:
        assert time.monotonic() < deadline, "no frame request"
        c.page.wait_for_timeout(20)


def test_new_frame_during_fetch(session, host):
    c, _ = session()
    held = hold_first_fetch(c)
    show(host, 1)
    wait_held(c, held)

    show(host, 2, {"fill": BLACK})
    host.wait("cancelled", seq=1)
    host.wait("presented", seq=2)
    release(held)
    c.page.wait_for_timeout(300)

    [img] = frames(c)
    assert probe(c, "[...p.urls]") == [img["src"]]
    assert len(replies(c)) == 1 and replies(c)[0]["type"] == "applied"
    # The old fetch made no object URL.
    assert len([e for e in probe(c, "p.log") if e[0] == "create"]) == 1


def test_close_during_fetch(session, host):
    c, _ = session()
    held = hold_first_fetch(c)
    show(host, 1)
    wait_held(c, held)

    host.send(cmd="close")
    host.wait("cancelled", seq=1)
    host.wait("closed")
    release(held)
    c.page.wait_for_timeout(300)
    assert frames(c) == []
    assert replies(c) == []
    assert probe(c, "p.urls.size") == 0


def test_new_frame_during_decode(session, host):
    c, _ = session()
    c.page.evaluate(HOLD_DECODE)
    show(host, 1)
    wait_page(c, "p.log.some((e) => e[0] === 'decode')")
    old = probe(c, "p.log.find((e) => e[0] === 'decode')[1]")

    c.page.evaluate("() => { window.__probe.gate = null; }")
    show(host, 2, {"fill": BLACK})
    host.wait("cancelled", seq=1)
    host.wait("presented", seq=2)
    [new] = frames(c)

    c.page.evaluate("window.__probe.release()")
    wait_page(c, f"p.log.some((e) => e[0] === 'decoded' && e[1] === {old!r})")
    c.page.wait_for_timeout(100)
    # The slow old decode does not replace the new image and sends nothing.
    assert frames(c) == [new]
    assert [m["type"] for m in replies(c)] == ["applied"]
    assert ["revoke", old] in probe(c, "p.log")
    assert probe(c, "[...p.urls]") == [new["src"]]


def test_close_during_decode(session, host):
    c, _ = session()
    show(host, 1)
    host.wait("presented", seq=1)

    c.page.evaluate(HOLD_DECODE)
    show(host, 2, {"fill": BLACK})
    wait_page(c, "p.log.filter((e) => e[0] === 'decode').length === 2")
    host.send(cmd="close")
    host.wait("cancelled", seq=2)
    host.wait("closed")
    wait_page(c, "document.querySelectorAll('img').length === 0")

    c.page.evaluate("window.__probe.release()")
    wait_page(c, "p.log.filter((e) => e[0] === 'decoded').length === 2")
    c.page.wait_for_timeout(100)
    # The black close page stays.
    assert frames(c) == []
    assert len(replies(c)) == 1
    assert probe(c, "p.urls.size") == 0


def test_disconnect_clears_the_image(session, host):
    c, _ = session()
    show(host, 1)
    host.wait("presented", seq=1)
    host.close()
    c.wait_status("Disconnected")
    assert frames(c) == []
    assert probe(c, "p.urls.size") == 0
