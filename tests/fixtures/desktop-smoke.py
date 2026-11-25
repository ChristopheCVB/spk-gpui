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
from livekit_fixture import livekit_server

ROOT = Path(__file__).resolve().parents[2]
WEB_REPO = "https://github.com/meeehdi-dev/spk.git"
WEB_REVISION = "43613954d119eae4fd30e25a1f969b4327c58e89"
EMAIL = "reader@spk.test"
ADMIN_EMAIL = "author@spk.test"
PASSWORD = "fixture-password-42"
FIRST_VOICE = "Smoke Voice A"
SECOND_VOICE = "Smoke Voice B"
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


def http(origin, cookies=None):
    client = build_opener(ProxyHandler({}), HTTPCookieProcessor(cookies))

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
        {"name": "Smoke Author", "email": ADMIN_EMAIL, "password": PASSWORD},
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
    voice = {
        name: admin("/api/admin/channels", {"name": name, "type": "voice"})["id"]
        for name in (FIRST_VOICE, SECOND_VOICE)
    }

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
                ("target", target, reader, "Smoke destination loaded", now),
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

    return post_live_alert, voice


@contextmanager
def voice_peer(origin, email, channel, web, log, env):
    cookies = CookieJar()
    account = http(origin, cookies)
    account("/api/auth/sign-in/email", {"email": email, "password": PASSWORD})
    cookie = "; ".join(f"{item.name}={item.value}" for item in cookies)
    script = """
const origin = process.env.PEER_ORIGIN;
const channelId = process.env.PEER_CHANNEL;
const sid = crypto.randomUUID();
const url = origin.replace(/^http/, "ws") + "/_ws";
const socket = new WebSocket(url, {headers: {Origin: origin, Cookie: process.env.PEER_COOKIE}});
const send = frame => socket.send(JSON.stringify({...frame, sid}));
socket.addEventListener("open", () => send({type: "voice:join", channelId, ready: true}));
socket.addEventListener("message", event => {
    const frame = JSON.parse(event.data);
    if (frame.type === "voice:joined") {
        send({type: "voice:state", cam: true, screen: true});
        console.log("joined");
    }
});
socket.addEventListener("error", event => {
    console.error(event.message);
    process.exit(1);
});
"""
    env = {**env, "PEER_ORIGIN": origin, "PEER_CHANNEL": channel, "PEER_COOKIE": cookie}
    with service(["node", "-e", script], log, web, env) as process:
        def joined():
            if process.poll() is not None:
                raise RuntimeError(f"Voice peer exited. See {log}")
            return "joined" in log.read_text()

        wait_for("the SPK Web voice peer", joined)
        yield


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

    def find_node(self, name, role=None, partial=False, within=None):
        self.ensure_running()
        nodes = walk(self.find_node(*within)) if within else self.nodes()
        for node in nodes:
            label = node.get_name() or ""
            if (name in label if partial else name == label) and (
                role is None or node.get_role_name() == role
            ):
                return node

    def find(self, name, role=None, partial=False, within=None):
        def probe():
            try:
                return self.find_node(name, role, partial, within)
            except GLib.Error:
                # A frame can replace part of the accessibility tree during traversal.
                return None

        return wait_for(repr(name), probe)

    def bounds(self, name, role=None, partial=False, within=None):
        node = self.find(name, role, partial, within)
        return node.get_component_iface().get_extents(Atspi.CoordType.WINDOW)

    def click(self, name, role=None, partial=False, button=1, within=None):
        bounds = self.bounds(name, role, partial, within)
        if bounds.width <= 0 or bounds.height <= 0:
            raise RuntimeError(f"Control is outside the window: {name}")
        self.click_at(bounds.x + bounds.width // 2, bounds.y + bounds.height // 2, button)

    def move(self, x, y):
        subprocess.run(["xdotool", "mousemove", "--window", self.window, str(x), str(y)],
                       check=True)

    def click_at(self, x, y, button=1):
        self.move(x, y)
        subprocess.run(["xdotool", "click", str(button)], check=True)

    def menu_order(self, *labels):
        positions = [self.bounds(label, None if label == "Admin" else "menu item").y
                     for label in labels]
        if positions != sorted(set(positions)):
            raise RuntimeError(f"Menu items are out of order: {labels}")

    def absent(self, name, role):
        if self.find_node(name, role) is not None:
            raise RuntimeError(f"Unexpected {role}: {name}")

    def check_disabled_menu_item(self, name):
        # gpui-kit skips disabled items and leaves the popup open when clicked.
        self.click(name, "menu item")
        self.find(name, "menu item")

    def screenshot(self, logs, filename):
        subprocess.run(["import", "-window", self.window, str(logs / filename)],
                       timeout=10, check=True)

    def channel_ellipsis(self, row):
        bounds = self.bounds(row, "push button", partial=True)
        self.move(bounds.x + 8, bounds.y + bounds.height // 2)
        label = f"Actions for {row}"
        action = self.bounds(label, "push button")
        row_bounds = self.bounds(row, "push button", partial=True)
        if action.x < row_bounds.x + row_bounds.width:
            raise RuntimeError(f"The ellipsis must follow the name of {row}")
        return label

    def open_participant_menu(self, person, context=False):
        if context:
            self.click(person, "panel", button=3)
        else:
            bounds = self.bounds(person, "panel")
            self.move(bounds.x + bounds.width // 2, bounds.y + bounds.height // 2)
            self.click(f"Actions for {person}", "push button", within=(person, "panel"))
        self.find("Profile", "menu item")

    def open_voice_channel_menu(self, channel, action, admin=False, context=False):
        if context:
            self.click(channel, "push button", button=3)
        else:
            self.click(self.channel_ellipsis(channel), "push button")
        if admin:
            self.menu_order(action, "Admin", "Rename", "Move up", "Move down", "Delete")
        else:
            self.find(action, "menu item")
            self.absent("Rename", "menu item")

    def role_choices(self, open_submenu=True):
        if open_submenu:
            self.click("Change role", "menu item")
        user = self.find("User", "menu item")
        submenu = user.get_parent()
        while submenu.get_role_name() != "menu":
            submenu = submenu.get_parent()
        admin = next(node for node in walk(submenu)
                     if node.get_role_name() == "menu item" and node.get_name() == "Admin")
        return admin, user

    def check_reader_menus(self, logs):
        target = "smoke-target"
        person = "Smoke Author"
        self.click(target, "push button", partial=True, button=3)
        self.menu_order("Mark read", "Mute notifications")
        self.absent("Rename", "menu item")
        self.click("Mute notifications", "menu item")
        target_action = self.channel_ellipsis(target)
        self.move(1000, 30)
        self.tab_to(target_action, "push button")
        self.key("Return")
        self.menu_order("Mark read", "Unmute notifications")
        self.click("Unmute notifications", "menu item")

        self.click(target, "push button", partial=True)
        self.visible_text("Smoke destination loaded")
        self.move(1000, 30)
        watch_buttons = [f"Watch {person}’s {feed}" for feed in ("camera", "screen share")]
        for label in watch_buttons:
            self.find(label, "push button")
        self.absent(f"Actions for {person}", "push button")
        row = self.bounds(person, "panel")
        self.move(row.x + row.width // 2, row.y + row.height // 2)
        action = self.bounds(f"Actions for {person}", "push button")
        if any(action.x + action.width > self.bounds(button, "push button").x
               for button in watch_buttons):
            raise RuntimeError("The participant ellipsis must precede the watch buttons")
        self.open_participant_menu(person, context=True)
        self.menu_order("Profile", "Send DM", "Mention", "Copy name")
        self.absent("Disconnect from voice", "menu item")
        self.absent("Volume…", "menu item")
        self.click("Mention", "menu item")
        self.visible_text("@Smoke Author")
        self.open_participant_menu(person)
        self.screenshot(logs, "participant-menu-dark.png")
        self.click("Profile", "menu item")
        self.find("Send DM", "push button")
        self.key("Escape")
        self.open_participant_menu(person, context=True)
        self.click("Send DM", "menu item")
        self.visible_text(f"Message @{person}")

        self.open_voice_channel_menu(FIRST_VOICE, "Join")
        self.key("Escape")
        self.open_voice_channel_menu(FIRST_VOICE, "Join", context=True)
        self.click("Join", "menu item")
        self.find("Leave voice", "push button")
        self.click(SECOND_VOICE, "push button", button=3)
        self.find("Switch", "menu item")
        self.key("Escape")
        self.open_participant_menu(person)
        self.menu_order("Profile", "Send DM", "Copy name", "Mute for me", "Volume…")
        self.absent("Mention", "menu item")
        self.click("Mute for me", "menu item")
        self.open_participant_menu(person, context=True)
        self.find("Unmute for me", "menu item")
        self.click("Volume…", "menu item")
        self.find("Mic volume", "slider")
        self.find("Screen Share volume", "slider")
        self.key("Escape")
        person_action = f"Actions for {person}"
        sidebar_row = (person, "panel")
        self.tab_to(person_action, "push button", within=sidebar_row)
        self.key("Return")
        self.find("Unmute for me", "menu item")
        self.key("Escape")
        wait_for("focus to return to the participant ellipsis",
                 lambda: self.focused(person_action, "push button", within=sidebar_row))
        self.open_voice_channel_menu(FIRST_VOICE, "Leave voice", context=True)
        self.key("Escape")
        self.open_voice_channel_menu(FIRST_VOICE, "Leave voice")
        self.click("Leave voice", "menu item")
        self.find(f"Join {FIRST_VOICE}", "push button")

    def check_admin_menus(self, logs, admin, server):
        person = "Smoke Reader"
        self.find("smoke-target, 1 unread, 0 pings", "push button")
        self.click("smoke-target", "push button", partial=True, button=3)
        self.menu_order("Mark read", "Mute notifications", "Admin", "Rename", "Move up", "Move down", "Delete")
        self.check_disabled_menu_item("Move down")
        self.screenshot(logs, "text-channel-menu-dark.png")
        self.click("Mark read", "menu item")
        self.find("smoke-target", "push button")
        text_action = self.channel_ellipsis("smoke-target")
        self.click(text_action, "push button")
        self.menu_order("Mark read", "Mute notifications", "Admin", "Rename", "Move up", "Move down", "Delete")
        self.click("Delete", "menu item")
        self.visible_text("Delete channel")
        self.click("Cancel", "push button")
        self.find("smoke-target", "push button", partial=True)
        self.click("smoke-target", "push button", partial=True, button=3)
        self.click("Move up", "menu item")
        wait_for("the Text Channel to move above smoke-chat",
                 lambda: self.bounds("smoke-target", "push button", partial=True).y
                 < self.bounds("smoke-chat", "push button", partial=True).y)

        voice_action = self.channel_ellipsis(FIRST_VOICE)
        ellipsis = self.bounds(voice_action, "push button")
        phone = self.bounds(f"Join {FIRST_VOICE}", "push button")
        if ellipsis.x + ellipsis.width > phone.x:
            raise RuntimeError("The Voice Channel ellipsis must precede the phone button")
        self.open_voice_channel_menu(FIRST_VOICE, "Join", admin=True, context=True)
        self.key("Escape")
        self.open_voice_channel_menu(FIRST_VOICE, "Join", admin=True)
        self.screenshot(logs, "voice-channel-menu-dark.png")
        self.click("Join", "menu item")
        self.find("Leave voice", "push button")
        self.open_voice_channel_menu(FIRST_VOICE, "Leave voice", admin=True, context=True)
        self.click("Leave voice", "menu item")
        self.find(f"Join {FIRST_VOICE}", "push button")

        def account():
            return next(user for user in admin("/api/users") if user["displayName"] == person)

        self.open_participant_menu(person, context=True)
        self.menu_order("Profile", "Send DM", "Copy name", "Admin", "Change role", "Move to",
                        "Disconnect from voice", "Ban")
        self.click("Move to", "menu item")
        self.find(SECOND_VOICE, "menu item")
        self.absent(FIRST_VOICE, "menu item")
        self.key("Escape")
        self.key("Escape")
        self.open_participant_menu(person)
        _, user_choice = self.role_choices()
        if not user_choice.get_state_set().contains(Atspi.StateType.CHECKED):
            raise RuntimeError("The current User role is unchecked")
        role_endpoint = f"/api/admin/users/{account()['id']}/role"
        admin(role_endpoint, {"role": "admin"})
        wait_for("the open role menu to show the promotion",
                 lambda: self.role_choices(open_submenu=False)[0].get_state_set().contains(Atspi.StateType.CHECKED))
        self.click("User", "menu item")
        wait_for("the open role menu to accept the new role choice", lambda: account()["role"] == "user")
        self.visible_text("Role updated")
        self.open_participant_menu(person)
        self.role_choices()
        wait_for("the menu to show the restored User role",
                 lambda: self.role_choices(open_submenu=False)[1].get_state_set().contains(Atspi.StateType.CHECKED))
        admin_choice, _ = self.role_choices(open_submenu=False)
        self.screenshot(logs, "change-role-dark.png")
        bounds = admin_choice.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
        self.click_at(bounds.x + bounds.width // 2, bounds.y + bounds.height // 2)
        wait_for("the sidebar promotion to reach SPK", lambda: account()["role"] == "admin")
        self.visible_text("Role updated")
        self.open_participant_menu(person, context=True)
        admin_choice, _ = self.role_choices()
        if not admin_choice.get_state_set().contains(Atspi.StateType.CHECKED):
            raise RuntimeError("The updated Admin role is unchecked")
        self.key("Escape")
        self.key("Escape")

        self.open_participant_menu(person, context=True)
        self.click("Ban", "menu item")
        self.visible_text(f"Ban {person}")
        self.find("Ban reason", "entry")
        self.screenshot(logs, "ban-participant-dark.png")
        self.click("Cancel", "push button")
        wait_for("focus to return from the Ban confirmation",
                 lambda: self.focused(f"Actions for {person}", "push button",
                                      within=(person, "panel")))
        if account()["banned"]:
            raise RuntimeError("Cancelling Ban changed the participant’s account")
        self.open_participant_menu(person)
        self.click("Ban", "menu item")
        self.enter("Ban reason", "entry", "Smoke test confirmation")
        self.click("Ban", "push button")
        wait_for("the sidebar ban to reach SPK", lambda: account()["banned"])
        self.visible_text("User banned")
        wait_for("the banned participant to leave the sidebar",
                 lambda: self.find_node(person, "panel") is None)

        os.killpg(server.pid, signal.SIGTERM)
        self.visible_text("Reconnecting to live updates")
        self.click(FIRST_VOICE, "push button", button=3)
        self.check_disabled_menu_item("Join")

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

    def focused(self, name, role, expected=True, within=None):
        try:
            node = self.find_node(name, role, within=within)
            is_focused = node is not None and node.get_state_set().contains(Atspi.StateType.FOCUSED)
            return is_focused == expected
        except GLib.Error:
            return False

    def tab_to(self, name, role, within=None):
        if self.focused(name, role, within=within):
            return
        for _ in range(50):
            self.key("Tab")
            if self.focused(name, role, within=within):
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

    def sign_in(self, email, nickname=None):
        self.find("Sign in to SPK")
        self.window = subprocess.check_output(["xdotool", "search", "--name", "^SPK$"], text=True).strip()
        subprocess.run(["xdotool", "windowfocus", self.window], check=True)
        self.enter("Email", "entry", email)
        self.enter("Password", "password text", PASSWORD)
        self.click("Sign in", "push button")
        if nickname:
            self.enter("Nickname", "entry", nickname)
            self.click("Continue", "push button")
        for channel in (FIRST_VOICE, SECOND_VOICE):
            self.find(channel, "push button")

    def check(self, post_live_alert, logs):
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
            self.screenshot(logs, "webhook-dark.png")
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
    directory.mkdir(exist_ok=True)
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
    appearance = Path(env["XDG_CONFIG_HOME"]) / "spk-gpui" / "appearance"
    appearance.parent.mkdir()
    appearance.write_text("Dark")
    env.update(
        PATH=f"{bin_dir}:{env['PATH']}", BROWSER=str(launcher), SPK_SMOKE_LAUNCHES=str(launches),
        XDG_SESSION_TYPE="x11", PULSE_SERVER=f"unix:{directory / 'silent-pulse'}",
        PIPEWIRE_REMOTE="spk-smoke-no-audio",
    )
    return env


@contextmanager
def silent_audio(directory, log, env):
    config = directory / "smoke.pa"
    config.write_text(
        f"load-module module-native-protocol-unix socket={directory / 'silent-pulse'} auth-anonymous=1\n"
        "load-module module-null-sink sink_name=spk_smoke rate=48000 channels=2\n"
        "set-default-sink spk_smoke\n"
        "set-default-source spk_smoke.monitor\n"
    )
    with service(["pulseaudio", "-n", "-F", config, "--daemonize=no",
                  "--use-pid-file=no", "--exit-idle-time=-1"], log, directory, env) as server:
        def ready():
            if server.poll() is not None:
                raise RuntimeError(f"The silent PulseAudio server exited. See {log}")
            status = subprocess.run(["pactl", "get-default-sink"], env=env,
                                    capture_output=True, text=True, timeout=3)
            return status.stdout.strip() == "spk_smoke"

        wait_for("the silent audio device", ready)
        yield


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


@contextmanager
def desktop_session(binary, launches, log, env):
    client_env = {key: value for key, value in env.items()
                  if key.lower() not in ("http_proxy", "https_proxy", "all_proxy")}
    with service([binary], log, ROOT, client_env) as client:
        desktop = Desktop(client, launches)
        try:
            yield desktop
            desktop.ensure_launches_checked()
        except Exception:
            with suppress(OSError, subprocess.SubprocessError):
                subprocess.run(["import", "-window", "root", str(log.with_suffix(".png"))],
                               timeout=10, check=True)
            with suppress(OSError, GLib.Error), log.with_suffix(".txt").open("w") as tree:
                for node in desktop.nodes():
                    print(node.get_role_name(), repr(node.get_name()), file=tree)
            raise


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
    subprocess.run(["xset", "b", "off"], check=True)
    subprocess.run(
        ["gdbus", "call", "--session", "--dest", "org.a11y.Bus", "--object-path", "/org/a11y/bus",
         "--method", "org.freedesktop.DBus.Properties.Set", "org.a11y.Status", "IsEnabled", "<true>"],
        check=True, stdout=subprocess.DEVNULL,
    )
    Atspi.init()
    with (livekit_server(attempt, logs / "livekit.log") as (signaling, key, secret),
          silent_audio(attempt, logs / "pulse.log", env)):
        livekit = f"ws://127.0.0.1:{signaling}"
        web_env.update(NUXT_LIVEKIT_API_KEY=key, NUXT_LIVEKIT_API_SECRET=secret,
                       NUXT_PUBLIC_LIVEKIT_URL=livekit)
        with service(["node", "node_modules/nuxt/bin/nuxt.mjs", "dev", "--host", "127.0.0.1",
                      "--port", str(port)], server_log, web, web_env) as server:
            post_live_alert, voice = seed(origin, database, server_log, server)
            binary = desktop_binary(work, logs, {**env, "SPK_URL": origin, "LIVEKIT_URL": livekit})
            with (voice_peer(origin, ADMIN_EMAIL, voice[FIRST_VOICE], web, logs / "author-peer.log", env),
                  desktop_session(binary, launches, logs / "reader.log", env) as desktop):
                desktop.sign_in(EMAIL, "Smoke Reader")
                desktop.check(post_live_alert, logs)
                desktop.check_reader_menus(logs)

            admin_launches = logs / "admin-browser-launches.log"
            admin_launches.touch()
            admin_env = isolated_env(attempt / "admin", admin_launches)
            admin_env["PULSE_SERVER"] = env["PULSE_SERVER"]
            admin = http(origin)
            admin("/api/auth/sign-in/email", {"email": ADMIN_EMAIL, "password": PASSWORD})
            with (voice_peer(origin, EMAIL, voice[FIRST_VOICE], web, logs / "reader-peer.log", env),
                  desktop_session(binary, admin_launches, logs / "admin.log", admin_env) as desktop):
                desktop.sign_in(ADMIN_EMAIL)
                desktop.check_admin_menus(logs, admin, server)
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
