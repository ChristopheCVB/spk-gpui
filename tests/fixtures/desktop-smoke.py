"""Exercise the Linux desktop against a pinned, disposable SPK Web instance."""

import argparse
from contextlib import contextmanager, suppress
import csv
import errno
import fcntl
from http.cookiejar import CookieJar
import io
import json
import os
from pathlib import Path
import re
import signal
import socket
import sqlite3
import subprocess
import tempfile
import time
from urllib.error import HTTPError, URLError
from urllib.parse import parse_qs, urlsplit
from urllib.request import HTTPCookieProcessor, ProxyHandler, Request, build_opener

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi, GLib

ROOT = Path(__file__).resolve().parents[2]
WEB_REPO = "https://github.com/meeehdi-dev/spk.git"
WEB_REVISION = "43613954d119eae4fd30e25a1f969b4327c58e89"
EMAIL = "reader@spk.test"
PASSWORD = "fixture-password-42"
GITHUB = "https://github.com"
PUSH_REPO = f"{GITHUB}/spk-smoke/demo"
PUSH_SENDER = {"login": "Push Sender", "html_url": f"{GITHUB}/push-sender"}
PUSH_COMMITS = (
    {"id": "8a072c0", "url": f"{PUSH_REPO}/commit/8a072c0", "message": "Add tests",
     "author": {"name": "Alice $Team$ Smith", "username": "alice-author"}},
    {"id": "c9407ec", "url": f"{PUSH_REPO}/commit/c9407ec", "message": "Clean up",
     "author": {"name": "Bob 🚀 Author", "username": "bob-author"}},
    {"id": "d651c1e", "url": f"{PUSH_REPO}/commit/d651c1e", "message": "Update comments",
     "author": {"name": "Local Author"}},
)
PUSH_LINKS = (
    ("8a072c0", f"{PUSH_REPO}/commit/8a072c0"),
    ("Alice $Team$ Smith", f"{GITHUB}/alice-author"),
    ("c9407ec", f"{PUSH_REPO}/commit/c9407ec"),
    ("Bob 🚀 Author", f"{GITHUB}/bob-author"),
    ("d651c1e", f"{PUSH_REPO}/commit/d651c1e"),
    ("Push Sender", PUSH_SENDER["html_url"]),
)


def wait_for(description, probe, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if result := probe():
            return result
        time.sleep(0.15)
    raise RuntimeError(f"Timed out waiting for {description}")


@contextmanager
def service(args, log, cwd, env):
    with log.open("wb") as output:
        process = subprocess.Popen(
            args, cwd=cwd, env=env, stdout=output, stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        try:
            yield process
        finally:
            try:
                os.killpg(process.pid, signal.SIGTERM)
                process.wait(timeout=5)
            except ProcessLookupError:
                process.wait()
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()


def web_checkout(work, logs):
    web = work / "spk-web"
    if web.is_symlink():
        raise RuntimeError(f"Expected a fixture checkout, found a symlink: {web}")
    if not web.exists():
        subprocess.run(["git", "init", "-q", web], check=True)
        subprocess.run(["git", "-C", web, "remote", "add", "origin", WEB_REPO], check=True)
    if not (web / ".git").is_dir() or subprocess.check_output(
        ["git", "-C", web, "remote", "get-url", "origin"], text=True,
    ).strip() != WEB_REPO:
        raise RuntimeError(f"Expected a fixture checkout of {WEB_REPO}: {web}")
    if subprocess.check_output(["git", "-C", web, "status", "--porcelain", "--untracked-files=no"]):
        raise RuntimeError(f"Pinned SPK Web checkout has local changes: {web}")
    revision = subprocess.run(
        ["git", "-C", web, "rev-parse", "--verify", "HEAD"],
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
    ).stdout.strip()
    if revision != WEB_REVISION:
        subprocess.run(["git", "-C", web, "fetch", "--depth=1", "origin", WEB_REVISION], check=True)
        subprocess.run(["git", "-C", web, "checkout", "--detach", WEB_REVISION], check=True)
    package_manager = json.loads((web / "package.json").read_text())["packageManager"]
    with (logs / "pnpm.log").open("wb") as output:
        subprocess.run(
            ["npm", "exec", "--yes", f"--package={package_manager}", "--", "pnpm",
             "--dir", web, "install", "--frozen-lockfile"],
            cwd=ROOT, stdout=output, stderr=subprocess.STDOUT, check=True,
        )
    return web


def http(origin):
    client = build_opener(ProxyHandler({}), HTTPCookieProcessor(CookieJar()))

    def request(path, data=None, headers=None):
        request = Request(
            origin + path, data=json.dumps(data).encode() if data is not None else None,
            headers={"Origin": origin, "Content-Type": "application/json", **(headers or {})},
        )
        try:
            with client.open(request, timeout=5) as response:
                body = response.read()
                return json.loads(body) if body else None
        except HTTPError as error:
            raise RuntimeError(f"SPK fixture request {request.method} {path.split('/')[1:3]} failed: "
                               f"HTTP {error.code} {error.read().decode()}") from error

    return request


def seed(origin, database, log, process):
    admin = http(origin)

    def first_invite():
        if process.poll() is not None:
            raise RuntimeError(f"SPK Web exited. See {log}")
        match = re.search(r"/signup\?invite=([\w-]+)", log.read_text(errors="replace"))
        if match:
            try:
                admin("/api/health")
            except URLError:
                return None
            return match[1]

    token = wait_for("SPK Web health and first admin invite", first_invite, timeout=90)
    owner = admin(
        "/api/auth/sign-up/email",
        {"name": "Smoke Author", "email": "author@spk.test", "password": PASSWORD},
        {"X-Invite-Token": token},
    )["user"]["id"]
    admin("/api/profile/nickname", {"displayName": "Smoke Author"})
    signup = admin("/api/invites", {"note": "Smoke reader"})["url"]
    reader = http(origin)(
        "/api/auth/sign-up/email",
        {"name": "Smoke Reader", "email": EMAIL, "password": PASSWORD},
        {"X-Invite-Token": parse_qs(urlsplit(signup).query)["invite"][0]},
    )["user"]["id"]
    source = admin("/api/admin/channels", {"name": "smoke-chat", "type": "text"})["id"]
    target = admin("/api/admin/channels", {"name": "smoke-target", "type": "text"})["id"]
    for name in ("Smoke Voice A", "Smoke Voice B"):
        admin("/api/admin/channels", {"name": name, "type": "voice"})

    # Seed exact Markdown through the pinned schema so the fixture makes no link-preview requests.
    now = int(time.time() * 1000)
    with sqlite3.connect(database) as db:
        db.execute("PRAGMA foreign_keys = ON")
        db.executemany(
            "INSERT INTO messages(id, channel_id, user_id, body, created_at) VALUES (?, ?, ?, ?, ?)",
            [
                ("pin-a", source, owner, "First pinned smoke message", now - 3000),
                ("pin-b", source, owner, "Second pinned smoke message", now - 2000),
                ("rich", source, owner,
                 f"**Smoke bold** and *Smoke italic* with `smoke code`, "
                 f"[Smoke link](/c/{target}), and <@{reader}>.\n\n"
                 "||[Open smoke destination 👩🏽‍💻][destination]||\n\n"
                 "**Artwork 👍🏽** beside 🇫🇷 and 1️⃣, with unsupported 😺‍🌵 and literal `🙂`.\n\n"
                 "```\n🙂\n```\n\n"
                 f"[destination]: /c/{target}",
                 now - 1000),
                ("large-emoji", source, owner, "  🔥 🔥 🔥 🔥 🔥 🔥  ", now - 900),
                ("small-emoji", source, owner, "🥳🥳🥳🥳🥳🥳🥳", now - 800),
                ("target", target, owner, "Smoke destination loaded", now),
            ],
        )
        db.execute("INSERT INTO message_mentions(message_id, user_id) VALUES ('rich', ?)", (reader,))
    for message in ("pin-a", "pin-b"):
        admin(f"/api/messages/{message}/pin", {"on": True})

    webhook = admin("/api/webhooks", {"name": "Smoke Webhook", "icon": "webhook", "private": False})
    endpoint = urlsplit(webhook["url"])
    admin(endpoint.path, {
        "content": "Smoke webhook body 🧪",
        "embeds": [{"title": "Smoke deploy alert", "description": "Smoke embed body 🔔"}],
    })
    admin(endpoint.path, {"embeds": [{
        "title": "Smoke alert with no profile",
        "author": {"name": "Unlinked Author", "url": "javascript:alert(1)"},
    }]})
    repository = {"name": "demo", "full_name": "spk-smoke/demo", "html_url": PUSH_REPO}
    sender = PUSH_SENDER.copy()
    if avatar := os.environ.get("SPK_SMOKE_WEBHOOK_AVATAR"):
        sender["avatar_url"] = avatar
    admin(endpoint.path, {
        "repository": repository, "ref": "refs/heads/main", "sender": sender,
        "head_commit": {"timestamp": "2026-11-24T09:15:00Z"},
        "commits": PUSH_COMMITS,
    }, {"X-GitHub-Event": "push"})

    def post_live_alert(event, field, login, url):
        admin(endpoint.path, {
            "repository": repository, "action": "opened", "sender": sender,
            field: {"number": 12, "title": "Check webhook links", "user": {
                "login": login, "html_url": url,
            }},
        }, {"X-GitHub-Event": event})

    return post_live_alert


def walk(node):
    if node is not None:
        yield node
        for index in range(node.get_child_count()):
            yield from walk(node.get_child_at_index(index))


class Desktop:
    def __init__(self, process, launches):
        self.process = process
        self.launches = launches
        self.checked_launches = []

    def nodes(self):
        for index in range(Atspi.get_desktop(0).get_child_count()):
            app = Atspi.get_desktop(0).get_child_at_index(index)
            if app and app.get_process_id() == self.process.pid:
                yield from walk(app)

    def ensure_launches_checked(self):
        launches = self.launches.read_text().splitlines()
        if launches != self.checked_launches:
            raise RuntimeError(f"Unexpected external launches: {launches[len(self.checked_launches):]}")

    def ensure_running(self):
        self.ensure_launches_checked()
        if self.process.poll() is not None:
            raise RuntimeError("SPK desktop exited during the smoke test")

    def find_node(self, name, role=None, partial=False):
        self.ensure_running()
        for node in self.nodes():
            label = node.get_name() or ""
            if (name in label if partial else name == label) and (
                role is None or node.get_role_name() == role
            ):
                return node

    def find(self, name, role=None, partial=False):
        def probe():
            try:
                return self.find_node(name, role, partial)
            except GLib.Error:
                # A frame can replace part of the accessibility tree during traversal.
                return None

        return wait_for(repr(name), probe)

    def click(self, name, role=None, partial=False):
        node = self.find(name, role, partial)
        bounds = node.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
        if bounds.width <= 0 or bounds.height <= 0:
            raise RuntimeError(f"Control is outside the window: {name}")
        self.click_at(bounds.x + bounds.width // 2, bounds.y + bounds.height // 2)

    def click_at(self, x, y):
        subprocess.run(["xdotool", "mousemove", "--window", self.window, str(x), str(y),
                        "click", "1"], check=True)

    def text_positions(self, phrases):
        self.ensure_running()
        image = subprocess.check_output(
            ["import", "-window", self.window, "-colorspace", "Gray", "-negate",
             "-resize", "200%", "png:-"], timeout=5,
        )
        result = subprocess.run(
            ["tesseract", "stdin", "stdout", "--psm", "11", "tsv"],
            input=image, capture_output=True, timeout=10,
        )
        if result.returncode:
            raise RuntimeError(result.stderr.decode())
        lines = {}
        for word in csv.DictReader(io.StringIO(result.stdout.decode()), delimiter="\t"):
            if word["text"].strip():
                key = (word["block_num"], word["par_num"], word["line_num"])
                lines.setdefault(key, []).append(word)
        matches = {}
        for words in lines.values():
            labels = [word["text"].casefold().strip(".,:;!?") for word in words]
            for phrase in phrases:
                parts = phrase.casefold().split()
                for index in range(len(labels) - len(parts) + 1):
                    if labels[index:index + len(parts)] == parts:
                        word = words[index]
                        matches[phrase] = ((int(word["left"]) + int(word["width"]) // 2) // 2,
                                           (int(word["top"]) + int(word["height"]) // 2) // 2)
        return matches

    def visible_text(self, *phrases):
        def probe():
            matches = self.text_positions(phrases)
            return matches if len(matches) == len(phrases) else None

        # GPUI's Markdown paragraphs do not expose their painted glyphs to AT-SPI.
        return wait_for(", ".join(phrases), probe)

    def enter(self, label, role, text):
        self.click(label, role)
        subprocess.run(["xdotool", "type", "--clearmodifiers", "--", text], check=True)

    def key(self, name):
        subprocess.run(["xdotool", "key", "--clearmodifiers", "--delay", "50", name],
                       check=True)

    def focused(self, name, role, expected=True):
        try:
            node = self.find_node(name, role)
            is_focused = node is not None and node.get_state_set().contains(Atspi.StateType.FOCUSED)
            return is_focused == expected
        except GLib.Error:
            return False

    def tab_to(self, name, role):
        if self.focused(name, role):
            return
        for _ in range(50):
            self.key("Tab")
            if self.focused(name, role):
                return
        raise RuntimeError(f"Tab did not reach {name}")

    def check_webhook_actions(self):
        label = "Actions for Smoke Webhook"

        def move(x, y):
            subprocess.run(["xdotool", "mousemove", "--window", self.window, str(x), str(y)],
                           check=True)

        def hidden():
            try:
                return self.find_node(label, "push button") is None
            except GLib.Error:
                return False

        move(700, 30)
        wait_for("the selected webhook to hide its ellipsis while idle", hidden)
        row = self.find("Smoke Webhook", "push button", partial=True)
        bounds = row.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
        move(bounds.x + 8, bounds.y + bounds.height // 2)
        action = self.find(label, "push button")
        action_bounds = action.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
        visible_row = self.find("Smoke Webhook", "push button")
        visible_bounds = visible_row.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
        if action_bounds.x + action_bounds.width > visible_bounds.x:
            raise RuntimeError("The webhook ellipsis must precede its primary button")
        move(700, 30)
        wait_for("the webhook ellipsis to hide after hover", hidden)

        self.tab_to(label, "push button")
        self.key("Tab")
        wait_for("focus to leave the webhook ellipsis", lambda: self.focused(label, "push button", expected=False))
        self.key("shift+Tab")
        wait_for("Shift+Tab to reach the webhook ellipsis",
                 lambda: self.focused(label, "push button"))
        self.key("Return")
        self.find("Copy link", "menu item")
        self.find(label, "push button")
        self.key("Escape")
        wait_for("Escape to return focus to the visible webhook ellipsis",
                 lambda: self.focused(label, "push button"))
        self.key("Tab")
        wait_for("focus to leave the webhook ellipsis", lambda: self.focused(label, "push button", expected=False))

        move(bounds.x + 8, bounds.y + bounds.height // 2)
        self.click(label, "push button")
        item = self.find("Copy link", "menu item")
        item_bounds = item.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
        move(item_bounds.x + item_bounds.width // 2, item_bounds.y + item_bounds.height // 2)
        self.find(label, "push button")
        self.key("Escape")
        wait_for("Escape to focus the ellipsis after opening with the mouse",
                 lambda: self.focused(label, "push button"))

        prior_focus = "New webhook"
        self.tab_to(prior_focus, "push button")
        move(bounds.x + 8, bounds.y + bounds.height // 2)
        self.click(label, "push button")
        self.find("Copy link", "menu item")
        for _ in range(3):
            self.key("Down")
        self.key("Return")
        wait_for("Enter to restore the focus from before the mouse-opened menu",
                 lambda: self.focused(prior_focus, "push button"))

    def external_link(self, url, activate):
        self.ensure_running()
        activate()
        launches = wait_for(url, lambda: self.launches.read_text().splitlines()[len(self.checked_launches):])
        if len(launches) != 1 or launches[0].split(maxsplit=1)[-1] != url:
            raise RuntimeError(f"Expected one launch of {url}, got {launches}")
        self.checked_launches.extend(launches)

    def check(self, post_live_alert, logs):
        self.find("Sign in to SPK")
        self.window = subprocess.check_output(["xdotool", "search", "--name", "^SPK$"], text=True).strip()
        subprocess.run(["xdotool", "windowfocus", self.window], check=True)
        self.enter("Email", "entry", EMAIL)
        self.enter("Password", "password text", PASSWORD)
        self.click("Sign in", "push button")
        self.enter("Nickname", "entry", "Smoke Reader")
        self.click("Continue", "push button")
        for channel in ("Smoke Voice A", "Smoke Voice B"):
            self.find(channel, "push button")
        self.find("smoke-target", "push button", partial=True)
        self.click("Smoke Webhook", "push button", partial=True)
        self.find("Smoke deploy alert", partial=True)
        self.visible_text("Smoke embed body")
        self.find("🧪")
        self.find("🔔")
        self.check_webhook_actions()
        self.find("Unlinked Author", "label")
        self.visible_text("Local Author", "spk-smoke/demo")
        for name, url in PUSH_LINKS:
            self.external_link(url, lambda: self.click(name, "link"))
        for name, url in PUSH_LINKS:
            self.tab_to(name, "link")
            self.external_link(url, lambda: self.key("Return"))
        for event, field, name, handle in (
            ("issues", "issue", "Issue Author", "issue-author"),
            ("pull_request", "pull_request", "PR Author", "pr-author"),
        ):
            url = f"{GITHUB}/{handle}"
            post_live_alert(event, field, name, url)
            self.external_link(url, lambda: self.click(name, "link"))
        if os.environ.get("SPK_SMOKE_WEBHOOK_AVATAR"):
            subprocess.run(["import", "-window", self.window, str(logs / "webhook-dark.png")],
                           timeout=10, check=True)
        self.click("smoke-chat", "push button", partial=True)
        self.visible_text("Smoke bold", "Smoke italic", "smoke code", "Smoke link", "@Smoke Reader")
        for emoji in ("👍🏽", "🇫🇷", "1️⃣"):
            self.find(emoji)
        mixed, seven, six = (
            self.find(emoji).get_component_iface().get_extents(Atspi.CoordType.WINDOW).width
            for emoji in ("👍🏽", "🥳", "🔥")
        )
        if not 0 < mixed < six or abs(mixed - seven) > 1:
            raise RuntimeError("Posted emoji should use inline sizing except in messages of one to six emoji")
        if any(node.get_name() in {"🙂", "😺‍🌵", "😺", "🌵", "👩🏽‍💻"} for node in self.nodes()):
            raise RuntimeError("Code, unsupported sequences, and unrevealed spoilers should not expose artwork")
        self.click("Show all 2 pinned messages", "toggle button")
        for message in ("First pinned smoke message", "Second pinned smoke message"):
            self.find(f"Smoke Author · {message}", partial=True)
        self.click("Close pinned messages", "toggle button")
        link = "Open smoke destination"
        if link in self.text_positions((link,)):
            raise RuntimeError("The spoiler link is readable before reveal")
        self.click("Spoiler: click to reveal", "push button")
        self.visible_text(link)
        self.click("👩🏽‍💻")
        self.visible_text("Smoke destination loaded")


def isolated_env(directory, launches):
    env = os.environ.copy()
    for name in ("WAYLAND_DISPLAY", "AT_SPI_BUS_ADDRESS", "PULSE_SINK", "PULSE_SOURCE"):
        env.pop(name, None)
    for category in ("CONFIG", "DATA", "CACHE", "RUNTIME", "STATE"):
        path = directory / category.lower()
        path.mkdir(mode=0o700)
        env[f"XDG_{category}_HOME" if category != "RUNTIME" else "XDG_RUNTIME_DIR"] = str(path)
    bin_dir = directory / "bin"
    bin_dir.mkdir()
    launcher = bin_dir / "browser"
    launcher.write_text('#!/bin/sh\nprintf "%s %s\\n" "${0##*/}" "${OPEN_RS_TARGET:-$*}" >> "$SPK_SMOKE_LAUNCHES"\n')
    launcher.chmod(0o700)
    for name in ("powershell.exe", "wslview", "xdg-open", "gio", "gnome-open",
                 "kde-open6", "kde-open5", "kde-open"):
        (bin_dir / name).symlink_to(launcher)
    alsa = directory / "alsa.conf"
    alsa.write_text("pcm.!default { type null }\nctl.!default { type hw card -1 }\n")
    appearance = Path(env["XDG_CONFIG_HOME"]) / "spk-gpui" / "appearance"
    appearance.parent.mkdir()
    appearance.write_text("Dark")
    env.update(
        PATH=f"{bin_dir}:{env['PATH']}", BROWSER=str(launcher), SPK_SMOKE_LAUNCHES=str(launches),
        XDG_SESSION_TYPE="x11", ALSA_CONFIG_PATH=str(alsa),
        PULSE_SERVER=f"unix:{directory / 'silent-pulse'}", PIPEWIRE_REMOTE="spk-smoke-no-audio",
    )
    return env


def desktop_binary(work, logs, env):
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=ROOT, env=env,
    ))
    target = work / "desktop-target"
    # Share Cargo's intermediate cache while keeping the loopback executable private.
    with (logs / "cargo.log").open("wb") as output:
        subprocess.run(
            ["cargo", "build", "--locked", "--bin", "spk-gpui", "--target-dir", target],
            cwd=ROOT, env={**env, "CARGO_BUILD_BUILD_DIR": metadata["build_directory"]},
            stdout=output, stderr=subprocess.STDOUT, check=True,
        )
    return target / "debug" / "spk-gpui"


def main(work, artifacts):
    artifacts.mkdir(parents=True, exist_ok=True)
    attempt = Path(tempfile.mkdtemp(prefix="run-", dir=work))
    logs = artifacts / attempt.name
    logs.mkdir()
    print(f"SPK smoke files: {attempt}\nSPK smoke evidence: {logs}", flush=True)
    launches = logs / "browser-launches.log"
    launches.touch()
    env = isolated_env(attempt, launches)
    web = web_checkout(work, logs)
    port_file = work / "port"
    with socket.socket() as listener:
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        saved_port = int(port_file.read_text()) if port_file.exists() else 0
        try:
            listener.bind(("127.0.0.1", saved_port))
        except OSError as error:
            if error.errno != errno.EADDRINUSE:
                raise
            listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    port_file.write_text(str(port))
    origin = f"http://127.0.0.1:{port}"
    database = attempt / "spk.db"
    server_log = logs / "spk-web.log"
    web_env = {key: value for key, value in env.items()
               if not key.startswith(("NUXT_", "BETTER_AUTH_"))}
    web_env.update({
        "TMPDIR": str(attempt), "BETTER_AUTH_URL": origin,
        "BETTER_AUTH_SECRET": os.urandom(32).hex(), "NUXT_DATABASE_PATH": str(database),
    })
    desktop_env = {**env, "SPK_URL": origin, "LIVEKIT_URL": "ws://127.0.0.1:7880"}
    client_env = {key: value for key, value in desktop_env.items()
                  if key.lower() not in ("http_proxy", "https_proxy", "all_proxy")}
    subprocess.run(["xset", "b", "off"], check=True)
    subprocess.run(
        ["gdbus", "call", "--session", "--dest", "org.a11y.Bus", "--object-path", "/org/a11y/bus",
         "--method", "org.freedesktop.DBus.Properties.Set", "org.a11y.Status", "IsEnabled", "<true>"],
        check=True, stdout=subprocess.DEVNULL,
    )
    Atspi.init()
    with service(["node", "node_modules/nuxt/bin/nuxt.mjs", "dev", "--host", "127.0.0.1",
                  "--port", str(port)], server_log, web, web_env) as server:
        post_live_alert = seed(origin, database, server_log, server)
        binary = desktop_binary(work, logs, desktop_env)
        with service([binary], logs / "desktop.log", ROOT, client_env) as client:
            desktop = Desktop(client, launches)
            try:
                desktop.check(post_live_alert, logs)
            except Exception:
                with suppress(OSError, subprocess.SubprocessError):
                    subprocess.run(["import", "-window", "root", str(logs / "failure.png")],
                                   timeout=10, check=True)
                with suppress(OSError, GLib.Error), (logs / "accessibility.txt").open("w") as tree:
                    for node in desktop.nodes():
                        print(node.get_role_name(), repr(node.get_name()), file=tree)
                raise
    desktop.ensure_launches_checked()
    print("SPK desktop smoke test passed")


def interrupt(_signal, _frame):
    raise KeyboardInterrupt


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, interrupt)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("work", type=Path, help="Task-owned directory to reuse across local checks")
    parser.add_argument("artifacts", type=Path, help="Separate directory for smoke test evidence")
    args = parser.parse_args()
    work = args.work.resolve()
    work.mkdir(parents=True, exist_ok=True)
    with (work / ".lock").open("w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        main(work, args.artifacts.resolve())
