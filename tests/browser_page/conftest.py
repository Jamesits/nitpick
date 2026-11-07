"""Fixtures: the `page_host` example of `nitpick-browser` and a Playwright browser.

Environment:
- `NITPICK_BROWSER`: Playwright engine, `chromium` (default), `firefox` or `webkit`.
- `NITPICK_BROWSER_CHANNEL`: installed browser for Chromium, for example `msedge` or
  `chrome`. Without it, Playwright needs its own build (`playwright install chromium`).
"""

import json
import os
import queue
import subprocess
import threading
import time
from pathlib import Path

import pytest
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[2]
CODE = "0042"
# Longest wait for one host event or page change. Each wait ends as soon as the
# condition holds.
TIMEOUT_S = 10.0


@pytest.fixture(scope="session")
def host_binary():
    subprocess.run(
        ["cargo", "build", "--quiet", "-p", "nitpick-browser", "--example", "page_host"],
        cwd=ROOT,
        check=True,
    )
    suffix = ".exe" if os.name == "nt" else ""
    target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    return target / "debug" / "examples" / f"page_host{suffix}"


class Host:
    """JSON-line events of one `page_host` process."""

    def __init__(self, binary):
        self.proc = subprocess.Popen(
            [binary, "--code", CODE],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
            encoding="utf-8",
        )
        self._lines = queue.Queue()
        self.seen = []
        # Called while `wait` has no event. The sync Playwright API runs route
        # and event handlers only inside its own calls; a test with routes
        # sets a page call here.
        self.pump = None
        threading.Thread(target=self._read, daemon=True).start()
        self.url = self.wait("listening")["url"]

    def _read(self):
        for line in self.proc.stdout:
            self._lines.put(json.loads(line))

    def wait(self, event, timeout=TIMEOUT_S, **fields):
        """Next event of this kind with these fields; earlier events are kept in `seen`."""
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise AssertionError(f"no {event} {fields}; events: {self.seen}")
            try:
                item = self._lines.get(timeout=min(remaining, 0.02) if self.pump else remaining)
            except queue.Empty:
                if self.pump:
                    self.pump()
                continue
            self.seen.append(item)
            if item["event"] == event and all(item.get(k) == v for k, v in fields.items()):
                return item

    def drain(self, settle=0.3):
        """Events that arrive within `settle` seconds."""
        items = []
        deadline = time.monotonic() + settle
        while (remaining := deadline - time.monotonic()) > 0:
            try:
                items.append(self._lines.get(timeout=remaining))
            except queue.Empty:
                break
        self.seen.extend(items)
        return items

    def send(self, **command):
        """One stdin command; see the `page_host` module documentation."""
        self.proc.stdin.write(json.dumps(command) + "\n")
        self.proc.stdin.flush()

    def close(self):
        self.proc.stdin.close()
        try:
            self.proc.wait(timeout=TIMEOUT_S)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            raise


@pytest.fixture
def host(host_binary):
    h = Host(host_binary)
    yield h
    h.close()


@pytest.fixture(scope="session")
def playwright_instance():
    with sync_playwright() as p:
        yield p


def launch(playwright_instance, args=()):
    engine = getattr(playwright_instance, os.environ.get("NITPICK_BROWSER", "chromium"))
    channel = os.environ.get("NITPICK_BROWSER_CHANNEL")
    return engine.launch(channel=channel, args=list(args)) if channel else engine.launch(args=list(args))


@pytest.fixture(scope="session")
def browser(playwright_instance):
    b = launch(playwright_instance)
    yield b
    b.close()


@pytest.fixture
def is_chromium():
    return os.environ.get("NITPICK_BROWSER", "chromium") == "chromium"


REJECTED = "The server rejected the connection"
VIEWPORT = {"width": 800, "height": 600}


class Client:
    """One page and the WebSocket messages that it sent."""

    def __init__(self, browser, url, dpr=1, viewport=VIEWPORT, init_script=None):
        self.context = browser.new_context(device_scale_factor=dpr, viewport=viewport)
        if init_script:
            self.context.add_init_script(init_script)
        self.page = self.context.new_page()
        self.sent = []
        self.page.on("websocket", lambda ws: ws.on("framesent", self._sent))
        self.page.goto(url)

    def _sent(self, payload):
        self.sent.append(json.loads(payload))

    def hellos(self):
        return [m for m in self.sent if m["type"] == "hello"]

    def status(self):
        return self.page.locator("#status").inner_text()

    def wait_status(self, text):
        self.page.locator("#status", has_text=text).wait_for(timeout=TIMEOUT_S * 1000)

    def authenticate(self, code=CODE):
        self.wait_status("Enter the code")
        self.page.fill("#code", code)
        self.page.click("#submit")

    def wait_hello(self, count):
        """The `count`-th `hello` of the page."""
        deadline = time.monotonic() + TIMEOUT_S
        while len(self.hellos()) < count:
            assert time.monotonic() < deadline, f"no hello {count}; sent: {self.sent}"
            self.page.wait_for_timeout(20)
        return self.hellos()[count - 1]

    def cdp(self, method, params):
        return self.context.new_cdp_session(self.page).send(method, params)

    def close(self):
        self.context.close()


@pytest.fixture
def client_factory(browser, host):
    clients = []

    def make(**kwargs):
        c = Client(browser, host.url, **kwargs)
        clients.append(c)
        return c

    yield make
    for c in clients:
        c.close()


def ready_client(host):
    """The `ready` event and the driver view that follows it."""
    ready = host.wait("ready")
    return host.wait("client", display=ready["display"])
