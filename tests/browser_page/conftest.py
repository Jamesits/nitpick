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
                item = self._lines.get(timeout=remaining)
            except queue.Empty:
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
