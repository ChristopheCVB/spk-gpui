"""Exercise the Linux desktop against a pinned, disposable SPK Web instance."""

import argparse
from contextlib import contextmanager, suppress
import csv
import errno
import fcntl
from http.cookiejar import CookieJar
from http.server import BaseHTTPRequestHandler, HTTPServer
import io
import json
import os
from pathlib import Path
import re
import signal
import socket
import sqlite3
import struct
import subprocess
import sys
import tempfile
from threading import Thread
import time
from urllib.error import HTTPError, URLError
from urllib.parse import parse_qs, urlsplit
from urllib.request import HTTPCookieProcessor, ProxyHandler, Request, build_opener
import wave

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi, GLib

# Keep the fixture import from writing a cache into the checkout.
sys.dont_write_bytecode = True
from livekit_fixture import livekit_server

ROOT = Path(__file__).resolve().parents[2]
WEB_REPO = "https://github.com/meeehdi-dev/spk.git"
WEB_REVISION = "43613954d119eae4fd30e25a1f969b4327c58e89"
R2_SIGNER = "server/utils/r2sign.ts"
R2_ORIGIN = b"https://${c.accountId}.r2.cloudflarestorage.com"
RADIO_DIRECTORY = "server/utils/radioApi.ts"
RADIO_STATIONS = [
    {"id": f"23456789-1111-4ccc-8aaa-123456789ab{index}", "name": f"Smoke {name}",
     "country": country, "codec": codec, "url": "https://radio.example.invalid/smoke.mp3"}
    for index, (name, country, codec) in enumerate(
        (("Jazz", "FR", "MP3"), ("Soul", "US", "AAC"), ("News", "GB", "MP3")),
    )
]
EMAIL = "reader@spk.test"
ADMIN_EMAIL = "author@spk.test"
PASSWORD = "fixture-password-42"
FIRST_VOICE = "Smoke Voice A"
SECOND_VOICE = "Smoke Voice B"
SOUND_NAME = "Smoke trimmed clip"
SOUND_ICON = "bell"
SOUND_RATE = 8_000
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
def service(args, log, cwd, env, stdin=None):
    with log.open("wb") as output:
        process = subprocess.Popen(
            args, cwd=cwd, env=env, stdout=output, stderr=subprocess.STDOUT,
            start_new_session=True, stdin=stdin,
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


def replace_web_file(web, path, staging, content):
    staging.write_bytes(content)
    staging.replace(web / path)


def radio_directory_source():
    # Leave SPK's authenticated endpoints, Favorites database, and voice commands in place.
    return f"""import type {{ RadioStation }} from '../../shared/utils/radio'
const stations: RadioStation[] = {json.dumps(RADIO_STATIONS)}
export const isStationId = (id: unknown): id is string =>
  typeof id === 'string' && /^[0-9a-f]{{8}}-[0-9a-f]{{4}}-[0-9a-f]{{4}}-[0-9a-f]{{4}}-[0-9a-f]{{12}}$/i.test(id)
export const searchRadioStations = async (query: string) =>
  stations.filter(station => station.name.toLowerCase().includes(query.trim().toLowerCase()))
export async function getRadioStation(id: string) {{
  const station = stations.find(station => station.id === id)
  if (!station) throw new Error('Unknown fixture station')
  return station
}}
export const getRadioStationSummaries = async (ids: string[]) =>
  stations.filter(station => ids.includes(station.id)).map(({{ id, name, country, codec }}) =>
    ({{ id, name, country, codec }}))
export const countRadioClick = async (_id: string) => {{}}
""".encode()


@contextmanager
def radio_directory(web, attempt):
    original = (web / RADIO_DIRECTORY).read_bytes()
    staging = attempt / "radioApi.ts"
    replace_web_file(web, RADIO_DIRECTORY, staging, radio_directory_source())
    try:
        yield
    finally:
        replace_web_file(web, RADIO_DIRECTORY, staging, original)


def web_checkout(work, logs, attempt):
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
    changes = subprocess.check_output(
        ["git", "-C", web, "status", "--porcelain", "--untracked-files=no"],
    )
    revision = subprocess.run(
        ["git", "-C", web, "rev-parse", "--verify", "HEAD"],
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
    ).stdout.strip()
    for change in changes.decode().splitlines():
        if revision != WEB_REVISION or change not in (f" M {R2_SIGNER}", f" M {RADIO_DIRECTORY}"):
            raise RuntimeError(f"Pinned SPK Web checkout has local changes: {web}")
        path = change[3:]
        original = subprocess.check_output(["git", "-C", web, "show", f"HEAD:{path}"])
        content = (web / path).read_bytes()
        if path == R2_SIGNER:
            # Recover the routing substitution, including the last attempt's ephemeral port.
            patched = re.escape(original).replace(
                re.escape(R2_ORIGIN), rb"http://127\.0\.0\.1:[1-9][0-9]{0,4}",
            )
            known_patch = original.count(R2_ORIGIN) == 1 and re.fullmatch(patched, content)
        else:
            known_patch = content == radio_directory_source()
        if not known_patch:
            raise RuntimeError(f"Pinned SPK Web checkout has local changes: {web}")
        replace_web_file(web, path, attempt / Path(path).name, original)
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


@contextmanager
def sound_storage(web, directory, log):
    directory.mkdir()

    class ObjectStorage(BaseHTTPRequestHandler):
        timeout = 5

        def object_path(self):
            key = re.fullmatch(r"/smoke/sounds/([0-9a-f-]{36})", urlsplit(self.path).path)
            if key:
                return directory / key[1]
            self.send_error(404)

        def do_PUT(self):
            path = self.object_path()
            if path is None:
                return
            length = int(self.headers.get("Content-Length", "0"))
            signed = parse_qs(urlsplit(self.path).query).get("X-Amz-SignedHeaders", [""])[0]
            if (self.headers.get("Content-Type") != "audio/wav"
                    or not 44 < length <= 3 * 1024 * 1024
                    or not {"content-length", "content-type"} <= set(signed.split(";"))):
                self.send_error(400)
                return
            body = self.rfile.read(length)
            if len(body) != length:
                self.send_error(400)
                return
            path.write_bytes(body)
            self.send_response(200)
            self.end_headers()

        def do_GET(self):
            path = self.object_path()
            if path is None:
                return
            if not path.is_file():
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header("Content-Type", "audio/wav")
            self.send_header("Content-Length", str(path.stat().st_size))
            self.end_headers()
            if self.command == "GET":
                self.wfile.write(path.read_bytes())

        do_HEAD = do_GET

        def log_message(self, _format, *_args):
            print(self.command, urlsplit(self.path).path, file=log, flush=True)

    original = (web / R2_SIGNER).read_bytes()
    if original.count(R2_ORIGIN) != 1:
        raise RuntimeError("Update the fixture R2 routing for the pinned SPK Web revision")
    with HTTPServer(("127.0.0.1", 0), ObjectStorage) as storage:
        origin = f"http://127.0.0.1:{storage.server_port}"
        thread = Thread(target=storage.serve_forever, daemon=True)
        # Keep the real presign and save handlers. Only their fixture object-store origin changes.
        staging = directory / "r2sign.ts"
        replace_web_file(web, R2_SIGNER, staging, original.replace(R2_ORIGIN, origin.encode()))
        thread.start()
        try:
            yield {
                "NUXT_R2_ACCOUNT_ID": "smoke",
                "NUXT_R2_BUCKET": "smoke",
                "NUXT_R2_ACCESS_KEY_ID": "smoke-access-key",
                "NUXT_R2_SECRET_ACCESS_KEY": "smoke-secret-key",
            }
        finally:
            storage.shutdown()
            thread.join()
            replace_web_file(web, R2_SIGNER, staging, original)


def sound_sources(directory):
    # Each section has its own pitch and uses integer samples to make the crop exact.
    for name, sections in (
        ("Short source.wav", [(3000, 2048, 500)]),
        ("Long source.wav", [(6100, 2048, 500), (8900, 4096, 1000),
                             (900, 8192, 2000), (100, 12288, 4000)]),
    ):
        with wave.open(str(directory / name), "wb") as wav:
            wav.setparams((1, 2, SOUND_RATE, 0, "NONE", "not compressed"))
            for ms, amplitude, frequency in sections:
                half_period = SOUND_RATE // frequency // 2
                period = struct.pack("<h", amplitude) * half_period
                period += struct.pack("<h", -amplitude) * half_period
                wav.writeframes(period * (ms * frequency // 1000))
    return directory / "Short source.wav", directory / "Long source.wav"


def verify_saved_sound(admin):
    clip = next((row for row in admin("/api/sounds") if row["name"] == SOUND_NAME), None)
    if clip is None or clip["icon"] != SOUND_ICON or clip["durationMs"] != 9800:
        raise RuntimeError(f"Unexpected saved Soundboard metadata: {clip}")
    url = admin(f"/api/sounds/{clip['id']}/file")["url"]
    with build_opener(ProxyHandler({})).open(url, timeout=5) as response:
        body = response.read()
    with wave.open(io.BytesIO(body), "rb") as wav:
        if (wav.getnchannels(), wav.getsampwidth(), wav.getframerate(), wav.getnframes()) != (
            1, 3, SOUND_RATE, 9_800 * SOUND_RATE // 1000,
        ):
            raise RuntimeError(f"Unexpected cropped WAV format or duration: {wav.getparams()}")
        pcm = wav.readframes(wav.getnframes())
    # 6.100–15.900 s at 150% gain spans 8.9 s of the middle tone and 0.9 s of the last tone.
    middle = (bytes.fromhex("000018") * 4 + bytes.fromhex("0000e8") * 4) * 8900
    tail = (bytes.fromhex("000030") * 2 + bytes.fromhex("0000d0") * 2) * 1800
    if pcm != middle + tail:
        raise RuntimeError("Stored PCM differs from the selected excerpt at 150% gain")


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
    alerts = admin("/api/admin/channels", {"name": "smoke-alerts", "type": "text"})["id"]
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

    return post_live_alert, voice, alerts, owner, reader


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
const {createInterface} = await import("node:readline");
createInterface({input: process.stdin}).on("line", line => send(JSON.parse(line)));
socket.addEventListener("open", () => send({type: "voice:join", channelId, ready: true}));
socket.addEventListener("message", event => {
    const frame = JSON.parse(event.data);
    if (frame.type === "voice:joined") {
        send({type: "voice:state", cam: true, screen: true});
        console.log("joined");
    } else if (frame.type === "voice:move" && frame.sid === sid) {
        send({type: "voice:join", channelId: frame.channelId, ready: true});
    }
});
socket.addEventListener("error", event => {
    console.error(event.message);
    process.exit(1);
});
"""
    env = {**env, "PEER_ORIGIN": origin, "PEER_CHANNEL": channel, "PEER_COOKIE": cookie}
    with service(["node", "--input-type=module", "-e", script], log, web, env,
                 stdin=subprocess.PIPE) as process:
        def joined():
            if process.poll() is not None:
                raise RuntimeError(f"Voice peer exited. See {log}")
            return "joined" in log.read_text()

        wait_for("the SPK Web voice peer", joined)
        def message(channel_id, body):
            process.stdin.write((json.dumps({
                "type": "message:send", "channelId": channel_id, "body": body,
                "nonce": os.urandom(16).hex(),
            }) + "\n").encode())
            process.stdin.flush()

        yield message


def walk(node):
    if node is not None:
        yield node
        for index in range(node.get_child_count()):
            yield from walk(node.get_child_at_index(index))


class Desktop:
    def __init__(self, process, launches, pulse_server):
        self.process = process
        self.launches = launches
        self.pulse_server = pulse_server
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

    def drag_horizontal(self, x, y, destination):
        self.move(x, y)
        subprocess.run(["xdotool", "mousedown", "1"], check=True)
        try:
            self.move((x + destination) // 2, y)
            self.move(destination, y)
        finally:
            subprocess.run(["xdotool", "mouseup", "1"], check=True)

    def menu_order(self, *labels):
        positions = [self.bounds(label, "menu item").y for label in labels]
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

    def check_toast_position(self, name, role=None):
        node = self.find(name, role)
        while node is not None and node.get_role_name() != "notification":
            node = node.get_parent()
        if node is None:
            raise RuntimeError(f"{name} should be inside a notification")
        toast = node.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
        geometry = subprocess.check_output([
            "xdotool", "getwindowgeometry", "--shell", self.window,
        ], text=True)
        width, height = map(int, re.findall(r"^(?:WIDTH|HEIGHT)=(\d+)$", geometry, re.MULTILINE))
        if abs(width - (toast.x + toast.width)) > 48 or toast.y < height / 2:
            raise RuntimeError(f"{name} should appear in the bottom right")
        return toast

    def check_channel_toast(self, logs):
        def background(toast):
            pixel = f"1x1+{toast.x + toast.width - 12}+{toast.y + toast.height // 2}"
            return subprocess.check_output(
                ["import", "-window", self.window, "-crop", pixel, "txt:-"], timeout=5,
            )

        result = "Channel reordered."
        self.check_toast_position(result)
        time.sleep(0.45)
        open_workspace = background(self.check_toast_position(result))
        self.click("New text channel", "push button")
        wait_for("Channel name to receive initial focus", lambda: self.focused("Channel name", "entry"))
        self.move(700, 30)
        time.sleep(0.25)
        if background(self.check_toast_position(result)) != open_workspace:
            raise RuntimeError("The dialog's dimming layer should stay behind the toast")
        self.screenshot(logs, "channel-toast-dark.png")
        self.key("ctrl+shift+F8")
        wait_for("the keyboard to dismiss the toast",
                 lambda: self.find_node(result) is None, timeout=2)
        wait_for("Channel name to retain focus", lambda: self.focused("Channel name", "entry"))
        self.click("Create", "push button")
        self.find("Channel names must be 1–32 characters.")
        self.click("Cancel", "push button")

    def channel_ellipsis(self, row):
        bounds = self.bounds(row, "push button", partial=True)
        self.move(bounds.x + 8, bounds.y + bounds.height // 2)
        label = f"Actions for {row}"
        action = self.bounds(label, "push button")
        row_bounds = self.bounds(row, "push button", partial=True)
        if action.x < row_bounds.x + row_bounds.width:
            raise RuntimeError(f"The ellipsis must follow the name of {row}")
        return label

    def check_chat_channel_ellipsis_fill(self, channel):
        label = self.channel_ellipsis(channel)
        row = self.bounds(channel, "push button", partial=True)
        action = self.bounds(label, "push button")
        self.move(action.x + action.width // 2, action.y + action.height // 2)
        time.sleep(0.15)

        def fills_match():
            left = row.x + 22
            right = action.x + 6
            crop = f"{right - left + 1}x1+{left}+{action.y + 24}"
            pixels = subprocess.check_output(
                ["import", "-window", self.window, "-crop", crop, "-depth", "8", "rgb:-"],
                timeout=5,
            )
            return len(pixels) == (right - left + 1) * 3 and pixels[:3] == pixels[-3:]

        wait_for(f"{channel} to share its hover fill with its ellipsis", fills_match, timeout=3)
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
            self.menu_order(action, "Rename", "Move up", "Move down", "Delete")
            self.absent("Admin", None)
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
        target_action = self.check_chat_channel_ellipsis_fill(target)
        self.move(1000, 30)
        self.tab_to(target_action, "push button")
        self.key("Return")
        self.menu_order("Mark read", "Unmute notifications")
        self.click("Unmute notifications", "menu item")

        self.click(target, "push button", partial=True)
        self.visible_text("Smoke destination loaded")
        self.check_chat_channel_ellipsis_fill(target)
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
        self.find(f"Switch to {SECOND_VOICE}", "push button")
        self.find("Smoke Reader", "panel", within=("Voice channels", "panel"))
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

    def check_radio_keyboard(self, logs):
        search = "Search radio stations"
        options = "Radio options"
        station = "Play Smoke Jazz for everyone"
        add = "Add Smoke Jazz to Favorites"
        remove = "Remove Smoke Jazz from Favorites"
        credit = "Stations from Radio Browser"
        stage = ("Voice controls", "tool bar")

        def open_radio(opener, within=None):
            self.tab_to(opener, "push button", within=within)
            self.key("Return")
            wait_for("initial Channel Radio search focus", lambda: self.focused(search, "entry"))

        def close_radio(opener, within=None):
            self.key("Escape")
            wait_for("focus to return to the Channel Radio opener",
                     lambda: self.focused(opener, "push button", within=within))
            self.absent(search, "entry")

        def tab_through_station_actions():
            wait_for("the station to be selectable", lambda: self.find(station, "push button")
                     .get_state_set().contains(Atspi.StateType.SENSITIVE))
            wait_for("the star to be enabled", lambda: self.find(add, "push button")
                     .get_state_set().contains(Atspi.StateType.SENSITIVE))
            self.tab_to(station, "push button")
            self.key("Tab")
            wait_for("the station star to be a separate Tab stop",
                     lambda: self.focused(add, "push button"))

        self.click(f"Join {FIRST_VOICE}", "push button")
        self.click("smoke-target", "push button", partial=True)
        open_radio("Channel Radio")
        self.find("Find a station to play in this channel.")
        self.key("ctrl+a")
        subprocess.run(["xdotool", "type", "--clearmodifiers", "--", "Smoke"], check=True)
        self.key("Return")
        tab_through_station_actions()
        close_radio("Channel Radio")

        self.click(f"Show {FIRST_VOICE} voice channel", "push button")
        open_radio(options, within=stage)
        tab_through_station_actions()
        self.key("space")
        wait_for("focus to stay on the star after adding a Favorite",
                 lambda: self.focused(remove, "push button"))
        self.absent("Stop for everyone", "push button")

        self.key("shift+Tab")
        wait_for("Shift+Tab to return to the station",
                 lambda: self.focused(station, "push button"))
        self.key("Return")
        self.find("Stop for everyone", "push button")
        slider = self.bounds("Radio volume (only you)", "slider")
        volume = self.bounds("Volume (only you)")
        if slider.y < volume.y + volume.height or slider.width < 250:
            raise RuntimeError("Radio volume should be a full-width slider below its label")
        self.find("Media Output", "combo box")
        self.external_link("https://www.radio-browser.info/", lambda: self.click(credit, "link"))
        results = self.bounds("Radio stations")
        if not 238 <= results.height <= 242:
            raise RuntimeError(f"Radio stations should be about 240 px high, got {results.height}")
        self.screenshot(logs, "radio-all-stations-dark.png")
        self.click("Favorites", "push button")
        self.find(remove, "push button")
        self.screenshot(logs, "radio-favorites-dark.png")
        close_radio(options, within=stage)

        geometry = subprocess.check_output(
            ["xdotool", "getwindowgeometry", "--shell", self.window], text=True,
        )
        normal = re.findall(r"^(?:WIDTH|HEIGHT)=(\d+)$", geometry, re.MULTILINE)
        try:
            subprocess.run(["xdotool", "windowsize", "--sync", self.window, "560", "440"], check=True)
            open_radio(options, within=stage)
            self.find(station, "push button")
            reduced = self.bounds("Radio stations")
            footer = self.bounds("Media Output", "combo box")
            if not 0 < reduced.height < results.height or footer.y + footer.height > 440:
                raise RuntimeError("Radio should shrink the station list and fit the footer at 560 × 440")
            self.screenshot(logs, "radio-minimum-dark.png")
            self.click("Stop for everyone", "push button")
            self.find("Find a station to play in this channel.")
            close_radio(options, within=stage)
        finally:
            subprocess.run(["xdotool", "windowsize", "--sync", self.window, *normal], check=True)
        self.click(f"Show {FIRST_VOICE} voice channel", "push button")
        self.click("Leave voice", "push button", within=stage)
        self.find(f"Join {FIRST_VOICE}", "push button")

    def check_radio_search(self):
        button = "push button"
        search = "Search radio stations"
        favorites = "Filter Favorites"
        jazz = "Play Smoke Jazz for everyone"
        soul = "Play Smoke Soul for everyone"
        add = "Add Smoke Jazz to Favorites"
        remove = "Remove Smoke Jazz from Favorites"

        def query(field, value):
            self.click(field, "entry")
            self.key("ctrl+a")
            subprocess.run(["xdotool", "type", "--clearmodifiers", "--", value], check=True)

        self.click(f"Join {FIRST_VOICE}", button)
        self.find(f"Switch to {SECOND_VOICE}", button)
        self.click("Radio options", button)
        self.click("Favorites", button)
        self.find("No saved stations.")
        self.click("All stations", button)
        query(search, " x ")
        self.key("Return")
        self.find("Search must contain 2–100 characters.")
        query(search, "Missing")
        self.click("Find stations", button)
        self.find("No stations found.")
        query(search, "Smoke")
        self.absent(jazz, button)
        self.find("No stations found.")
        self.key("Return")
        self.find(jazz, button)
        self.find(soul, button)
        self.click(add, button)
        self.find(remove, button)
        self.click("Favorites", button)
        self.find(jazz, button)
        self.absent(soul, button)
        query(favorites, "  JAZZ  ")
        self.find(jazz, button)
        query(favorites, "Soul")
        self.find("No Favorites match.")
        self.key("Return")
        self.find("No Favorites match.")
        self.click("All stations", button)
        self.find(jazz, button)
        self.find(soul, button)
        if Atspi.Text.get_text(self.find(search, "entry"), 0, -1) != "Soul":
            raise RuntimeError("Switching Radio tabs discarded the shared input")
        self.click("Favorites", button)
        self.find("No Favorites match.")
        self.key("Escape")
        wait_for("Channel Radio to close", lambda: self.find_node(favorites, "entry") is None)
        self.click("Radio options", button)
        self.find(jazz, button)
        self.find(soul, button)
        if Atspi.Text.get_text(self.find(search, "entry"), 0, -1) != "Smoke":
            raise RuntimeError("Radio did not reopen with the query for its retained results")
        wait_for("the Favorites refresh to enable the star", lambda: self.find(remove, button)
                 .get_state_set().contains(Atspi.StateType.SENSITIVE))
        self.click(remove, button)
        self.find(add, button)
        self.click("Favorites", button)
        self.find("No saved stations.")
        self.key("Escape")
        self.open_voice_channel_menu(FIRST_VOICE, "Leave voice")
        self.click("Leave voice", "menu item")
        self.find(f"Join {FIRST_VOICE}", button)

    def check_admin_menus(self, logs, admin, server, check_settings):
        person = "Smoke Reader"

        def check_text_menu():
            self.menu_order("Mark read", "Mute notifications", "Rename", "Move up", "Move down", "Delete")
            self.absent("Admin", None)

        self.find("smoke-target, 1 unread, 0 pings", "push button")
        self.click("smoke-target", "push button", partial=True, button=3)
        check_text_menu()
        self.check_disabled_menu_item("Move down")
        self.screenshot(logs, "text-channel-menu-dark.png")
        self.click("Mark read", "menu item")
        self.find("smoke-target", "push button")
        text_action = self.channel_ellipsis("smoke-target")
        self.click(text_action, "push button")
        check_text_menu()
        self.click("Delete", "menu item")
        self.visible_text("Delete channel")
        self.click("Cancel", "push button")
        self.find("smoke-target", "push button", partial=True)
        self.click("smoke-target", "push button", partial=True, button=3)
        self.click("Move up", "menu item")
        wait_for("the Text Channel to move above smoke-chat",
                 lambda: self.bounds("smoke-target", "push button", partial=True).y
                 < self.bounds("smoke-chat", "push button", partial=True).y)
        self.check_channel_toast(logs)
        check_settings()

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
        self.menu_order("Profile", "Send DM", "Copy name", "Change role", "Move to",
                        "Disconnect from voice", "Ban")
        self.absent("Admin", None)
        self.click("Move to", "menu item")
        self.find(SECOND_VOICE, "menu item")
        self.absent(FIRST_VOICE, "menu item")
        self.key("Escape")
        self.key("Escape")
        self.tab_to(f"Actions for {person}", "push button", within=(person, "panel"))
        self.key("Return")
        self.click("Move to", "menu item")
        destination = self.bounds(SECOND_VOICE, "menu item")
        self.move(destination.x + destination.width // 2, destination.y + destination.height // 2)
        self.key("Return")
        wait_for("the participant to move below the destination channel",
                 lambda: self.bounds(person, "panel").y > self.bounds(SECOND_VOICE, "push button").y)
        wait_for("focus to stay with the source Voice Channel",
                 lambda: self.focused(voice_action, "push button"))
        self.click("smoke-target", "push button")
        self.enter("Write a message", "entry", "Draft before moving participant")
        self.open_participant_menu(person, context=True)
        self.click("Move to", "menu item")
        self.click(FIRST_VOICE, "menu item")
        wait_for("the participant to move back above the second channel",
                 lambda: self.bounds(person, "panel").y < self.bounds(SECOND_VOICE, "push button").y)
        wait_for("focus to stay with the Message Composer",
                 lambda: self.focused("Write a message", "entry"))
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
        self.check_toast_position("Role updated.")
        self.open_participant_menu(person)
        self.role_choices()
        wait_for("the menu to show the restored User role",
                 lambda: self.role_choices(open_submenu=False)[1].get_state_set().contains(Atspi.StateType.CHECKED))
        admin_choice, _ = self.role_choices(open_submenu=False)
        self.screenshot(logs, "change-role-dark.png")
        bounds = admin_choice.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
        self.click_at(bounds.x + bounds.width // 2, bounds.y + bounds.height // 2)
        wait_for("the sidebar promotion to reach SPK", lambda: account()["role"] == "admin")
        self.check_toast_position("Role updated.")
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
        self.check_toast_position("User banned.")
        wait_for("the banned participant to leave the sidebar",
                 lambda: self.find_node(person, "panel") is None)

        self.open_voice_channel_menu(FIRST_VOICE, "Join", admin=True, context=True)
        os.killpg(server.pid, signal.SIGTERM)
        self.visible_text("Reconnecting to live updates")
        self.check_disabled_menu_item("Join")
        self.screenshot(logs, "voice-channel-disconnected-dark.png")
        self.key("Escape")
        self.click(FIRST_VOICE, "push button", button=3)
        self.check_disabled_menu_item("Join")

    def choose_sound_source(self, path):
        self.click("Choose audio file", "push button")

        def picker():
            result = subprocess.run(["xdotool", "search", "--onlyvisible", "--name", "^Open File$"],
                                    capture_output=True, text=True)
            return result.stdout.strip()

        dialog = wait_for("the native GTK file chooser", picker)
        subprocess.run(["xdotool", "windowfocus", "--sync", dialog], check=True)
        self.key("ctrl+l")
        subprocess.run(["xdotool", "type", "--clearmodifiers", "--delay", "10", "--",
                        f"{path.parent}/"], check=True)
        # Let GTK resolve the folder in its location entry before submitting it.
        time.sleep(0.3)
        self.key("Return")

        def source_row():
            try:
                return next((node for node in walk(Atspi.get_desktop(0))
                             if node.get_name() == path.name and node.get_role_name() == "table cell"), None)
            except GLib.Error:
                return None

        row = wait_for(f"{path.name} in the native file chooser", source_row)
        bounds = row.get_component_iface().get_extents(Atspi.CoordType.SCREEN)
        subprocess.run(["xdotool", "mousemove", str(bounds.x + bounds.width // 2),
                        str(bounds.y + bounds.height // 2), "click", "--repeat", "2", "1"], check=True)
        wait_for("the native file chooser to close", lambda: not picker())
        subprocess.run(["xdotool", "windowfocus", self.window], check=True)
        self.find("Source waveform")

    def activate_soundboard_button(self, name):
        def activate():
            try:
                node = self.find_node(name, "push button")
                if node and node.get_state_set().contains(Atspi.StateType.SENSITIVE):
                    action = node.get_action_iface()
                    return action and action.do_action(0)
            except GLib.Error:
                return None

        wait_for(f"{name} to accept activation", activate)

    def sound_timing(self, label):
        node = self.find(f"{label}: ", partial=True)
        return float(re.fullmatch(rf"{label}: (\d+\.\d{{3}}) s", node.get_name())[1])

    def expect_sound_selection(self, start, end):
        for label, value in (("Start", start), ("End", end), ("Duration", end - start)):
            self.find(f"{label}: {value:.3f} s")

    def check_late_waveform(self, waveform, left, ten_seconds):
        def painted_height(seconds):
            x = round(left + (ten_seconds - left) * seconds / 10)
            width = 12
            height = waveform.height - 8
            pixels = subprocess.check_output(
                ["import", "-window", self.window, "-crop",
                 f"{width}x{height}+{x}+{waveform.y + 4}", "-depth", "8", "rgb:-"],
                timeout=5,
            )
            colors = [pixels[i:i + 3] for i in range(0, len(pixels), 3)]
            return max(sum(color != column[0] for color in column)
                       for column in (colors[i::width] for i in range(width)))

        # Both strips are outside the initial selection. The louder tail must be drawn taller.
        if not 0 < painted_height(12) < painted_height(15.25):
            raise RuntimeError("The source waveform did not show its louder section after 15 seconds")

    def preview_tone_rms(self):
        # Record the private null sink while the preview is still within its 1000 Hz section.
        try:
            subprocess.run(
                ["parec", f"--server={self.pulse_server}", "--device=spk_smoke.monitor",
                 "--raw", "--format=s16le", "--rate=48000", "--channels=1", "--latency-msec=50"],
                stdout=subprocess.PIPE, check=True, timeout=3.5,
            )
        except subprocess.TimeoutExpired as error:
            pcm = error.stdout or b""
        else:
            raise RuntimeError("The silent Media Output monitor stopped recording unexpectedly")

        samples = [sample for (sample,) in struct.iter_unpack("<h", pcm[:len(pcm) & ~1])]
        window = 9600  # 200 ms, or 200 cycles of the selected tone.
        rms = 0.0
        for offset in range(0, len(samples) - window + 1, window):
            chunk = samples[offset:offset + window]
            crossings = 0
            negative = False
            for sample in chunk:
                if sample < -512:
                    negative = True
                elif sample > 512 and negative:
                    crossings += 1
                    negative = False
            if 180 <= crossings <= 220:
                rms = max(rms, (sum(sample ** 2 for sample in chunk) / window) ** 0.5)
        if rms == 0:
            raise RuntimeError("The silent Media Output did not play the selected 1000 Hz tone")
        return rms

    def check_soundboard_preview(self, logs):
        def position():
            try:
                node = self.find_node("Preview playhead")
                if node is not None:
                    return node.get_value_iface().get_current_value()
            except GLib.Error:
                # Playback updates can replace part of the tree during a query.
                pass
            return -1

        def play():
            self.click("Play selection", "push button")
            wait_for("the playhead to advance from the nonzero trim start",
                     lambda: 6.4 < position() < 15.9)

        def reset(start, timeout=15):
            wait_for(f"the preview to stop and reset to {start:.3f} s",
                     lambda: abs(position() - start) < 0.001
                     and self.find_node("Stop preview", "push button") is None, timeout=timeout)

        reset(6.1)
        play()
        self.screenshot(logs, "soundboard-preview-dark.png")
        amplified = self.preview_tone_rms()
        self.click("Stop preview", "push button")
        reset(6.1, timeout=3)

        self.replace_sound_field("Upload gain (%)", "100")
        play()
        baseline = self.preview_tone_rms()
        reset(6.1)
        ratio = amplified / baseline
        if not 1.4 <= ratio <= 1.6:
            raise RuntimeError(f"Expected 150% preview gain, measured {ratio:.2%}")
        self.replace_sound_field("Upload gain (%)", "150")

        play()
        handle = self.bounds("Trim end", "slider")
        waveform = self.bounds("Source waveform")
        self.drag_horizontal(handle.x + handle.width // 2, handle.y + handle.height // 2,
                             waveform.x + waveform.width - 1)
        self.expect_sound_selection(6.1, 16)
        reset(6.1, timeout=3)
        self.tab_to("Trim end", "slider")
        self.key("Left")
        self.expect_sound_selection(6.1, 15.9)

        play()
        self.replace_sound_field("Upload gain (%)", "125")
        reset(6.1, timeout=3)
        self.replace_sound_field("Upload gain (%)", "150")

    def replace_sound_field(self, label, value):
        self.click(label, "entry")
        self.key("ctrl+a")
        subprocess.run(["xdotool", "type", "--clearmodifiers", "--", value], check=True)

    def check_soundboard_upload(self, sources, logs, admin):
        self.click("Settings", "push button")
        self.activate_soundboard_button("Soundboard")
        self.find("No sounds yet.")
        self.choose_sound_source(sources[0])
        self.find("Source duration: 3.000 s")
        self.expect_sound_selection(0, 3)
        self.click("Discard", "push button")
        wait_for("the discarded waveform to clear", lambda: self.find_node("Source waveform") is None)

        self.choose_sound_source(sources[1])
        self.find("Source duration: 16.000 s")
        self.expect_sound_selection(0, 10)
        waveform = self.bounds("Source waveform")
        start = "Trim start"
        end = "Trim end"
        slider = "slider"
        start_handle = self.bounds(start, slider)
        self.check_late_waveform(waveform, start_handle.x + start_handle.width,
                                 self.bounds(end, slider).x)

        def center(label):
            bounds = self.bounds(label, slider)
            return bounds.x + bounds.width // 2, bounds.y + bounds.height // 2

        x, y = center(start)
        end_x, _ = center(end)
        self.drag_horizontal(x, y, x + (end_x - x) * 2 // 5)
        wait_for("only the start to move", lambda: 3.5 < self.sound_timing("Start") < 4.5)
        self.find("End: 10.000 s")
        x, y = center(end)
        self.drag_horizontal(x, y, waveform.x)
        self.find("Duration: 0.001 s")
        start_bounds = self.bounds(start, slider)
        end_bounds = self.bounds(end, slider)
        grip_x = (start_bounds.x + start_bounds.width + end_bounds.x) // 2
        grip_y = waveform.y + waveform.height - 10
        self.drag_horizontal(grip_x, grip_y, waveform.x)
        self.expect_sound_selection(0, 0.001)
        self.drag_horizontal(waveform.x + start_bounds.width, grip_y, grip_x)
        wait_for("the 1 ms selection to move back", lambda: 3.5 < self.sound_timing("Start") < 4.5)
        self.find("Duration: 0.001 s")
        x, y = center(end)
        self.drag_horizontal(x, y, waveform.x + waveform.width - 1)
        self.find("Duration: 10.000 s")
        if not 13.5 < self.sound_timing("End") < 14.5:
            raise RuntimeError("Moving the trim end failed to clamp the selection to 10 seconds")

        for right in (True, False, True):
            x, y = center(start)
            end_x, _ = center(end)
            destination = waveform.x + waveform.width - 1 if right else waveform.x
            self.drag_horizontal((x + end_x) // 2, y, destination)
            self.expect_sound_selection(*((6, 16) if right else (0, 10)))

        self.tab_to(start, slider)
        self.key("Right")
        self.expect_sound_selection(6.1, 16)
        self.tab_to(end, slider)
        self.key("Left")
        self.expect_sound_selection(6.1, 15.9)
        for label, minimum, maximum in ((start, 5.9, 15.899), (end, 6.101, 16.0)):
            value = self.find(label, slider).get_value_iface()
            if (abs(value.get_minimum_value() - minimum) > 0.00001
                    or abs(value.get_maximum_value() - maximum) > 0.00001):
                raise RuntimeError(f"{label} did not expose its current trim limits")
        self.replace_sound_field("Name", SOUND_NAME)
        self.replace_sound_field("Upload gain (%)", "150")
        self.click("Icon", "combo box")
        self.enter("Search icons", "entry", SOUND_ICON)
        self.click("", "list item", within=("", "list"))
        wait_for("the icon picker to close", lambda: self.find_node("Search icons", "entry") is None)
        self.check_soundboard_preview(logs)
        self.screenshot(logs, "soundboard-waveform-dark.png")
        self.click("Save", "push button")
        self.find(SOUND_NAME)
        wait_for("the saved waveform to clear", lambda: self.find_node("Source waveform") is None)
        verify_saved_sound(admin)
        self.click("Close settings", "push button")
        wait_for("Settings to close", lambda: self.find_node("Close settings", "push button") is None)

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

    def check_webhook_actions(self, logs):
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
        bounds = self.bounds("Smoke Webhook", "push button", partial=True)
        self.check_chat_channel_ellipsis_fill("Smoke Webhook")
        self.screenshot(logs, "webhook-actions-dark.png")
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

    def check_incoming_toast(self, logs, send_message, alerts, reader):
        self.click("smoke-chat", "push button", partial=True)
        self.find("Show all 2 pinned messages", "toggle button")
        send_message(alerts, f"<@{reader}> Incoming smoke alert")
        self.check_toast_position("Open", "push button")
        self.find("Smoke Author in #smoke-alerts", "label")
        time.sleep(0.45)
        self.screenshot(logs, "incoming-toast-dark.png")

    def check_upload_recovery(self):
        self.click("Write a message", "entry")
        image = subprocess.check_output(["convert", "-size", "8x8", "xc:blue", "png:-"])
        with subprocess.Popen(["xclip", "-quiet", "-selection", "clipboard", "-target",
                               "image/png"], stdin=subprocess.PIPE, stderr=subprocess.DEVNULL) as clipboard:
            clipboard.stdin.write(image)
            clipboard.stdin.close()
            try:
                wait_for("an image on the clipboard", lambda: b"image/png" in subprocess.run(
                    ["xclip", "-selection", "clipboard", "-target", "TARGETS", "-out"],
                    capture_output=True, timeout=2,
                ).stdout)
                self.key("ctrl+v")
                self.find("Retry", "push button")
                self.click("Remove file", "push button")
            finally:
                clipboard.terminate()

    def check_sidebar_resize_retries_save(self, config):
        def header_x():
            return self.bounds("Hide sidebar", "push button").x

        preference = config / "sidebar-width"
        preference.mkdir()
        initial = header_x()
        destination = initial + 112
        y = 240

        def move_sidebar(x):
            self.move(x - 12, y)
            wait_for("the sidebar to follow the drag", lambda: abs(header_x() - x) <= 2)

        self.move(initial - 14, y)
        subprocess.run(["xdotool", "mousedown", "1"], check=True)
        try:
            self.move(initial + 32, y)
            move_sidebar(destination)
        finally:
            subprocess.run(["xdotool", "mouseup", "1"], check=True)
        self.find("Could not save sidebar width.")
        preference.rmdir()

        self.move(header_x() - 14, y)
        subprocess.run(["xdotool", "mousedown", "1"], check=True)
        try:
            self.move(destination - 40, y)
            move_sidebar(initial)
            move_sidebar(destination)
            if preference.exists():
                raise RuntimeError("The sidebar width was saved before the drag ended")
            subprocess.run(["xdotool", "click", "3"], check=True)
            wait_for("the sidebar width to save on secondary-button release", preference.is_file)
            if abs(float(preference.read_text()) - 400) > 2:
                raise RuntimeError("The saved sidebar width does not match the resized width")
        finally:
            subprocess.run(["xdotool", "mouseup", "1"], check=True)

    def check_settings_toasts(self, logs, config, send_message, alerts, owner):
        notification_result = "Could not save notification settings."
        self.click("Settings", "push button")
        self.click("Notifications", "push button")
        for setting in ("Direct messages", "Mentions and replies"):
            self.click(setting, "toggle button")
            if self.find(setting, "toggle button").get_state_set().contains(Atspi.StateType.CHECKED):
                raise RuntimeError(f"{setting} notifications should be off")
        preferences, = config.glob("notifications/instance/*/account/*/preferences.json")
        preferences.unlink()
        preferences.mkdir()
        previews = "Show system notification previews"
        self.tab_to(previews, "toggle button")
        self.key("space")
        self.check_toast_position(notification_result)
        if not self.focused(previews, "toggle button"):
            raise RuntimeError("The toast took focus from the Settings control")
        started = time.monotonic()
        time.sleep(3.2)
        self.key("space")
        time.sleep(max(0, started + 5.5 - time.monotonic()))
        if sum(node.get_name() == notification_result for node in self.nodes()) != 1:
            raise RuntimeError("Repeating the action should refresh one toast")
        self.click("Appearance", "push button")
        (config / "density").mkdir()
        self.click("Compact", "radio button")
        density_result = "Could not save your density preference."
        self.check_toast_position(density_result)
        time.sleep(0.45)
        newest = self.check_toast_position(density_result)
        self.move(newest.x + newest.width // 2, newest.y + newest.height // 2)
        time.sleep(0.3)
        previous = self.check_toast_position(notification_result)
        if previous.y + previous.height > newest.y:
            raise RuntimeError("Distinct action results should expand into separate toasts")
        self.screenshot(logs, "settings-toasts-dark.png")
        self.move(700, 30)
        wait_for("the five-second action toasts to expire",
                 lambda: self.find_node(density_result) is None
                 and self.find_node(notification_result) is None, timeout=7)
        self.click("Shortcuts", "push button")
        self.click("Record shortcut for toggle mute", "push button")
        self.key("ctrl+shift+F8")
        self.find("Ctrl + Shift + F8")
        (config / "voice-preferences.json").mkdir()
        self.click("Save shortcut", "push button")
        shortcut_result = "Could not save voice preferences."
        self.check_toast_position(shortcut_result)
        self.key("ctrl+shift+F8")
        wait_for("the keyboard to dismiss the shortcut save failure",
                 lambda: self.find_node(shortcut_result) is None, timeout=2)
        self.find("Ctrl + Shift + F8")
        self.find("Save shortcut", "push button")
        (config / "voice-preferences.json").rmdir()
        self.click("Save shortcut", "push button")
        self.find("Record shortcut for toggle mute", "push button")
        self.click("Appearance", "push button")
        self.click("Comfortable", "radio button")
        self.check_toast_position(density_result)
        self.click("Close settings", "push button")
        wait_for("Settings to close", lambda: self.find_node("Close settings", "push button") is None)
        self.key("ctrl+shift+F8")
        self.find("Unmute", "toggle button")
        self.check_toast_position(density_result)
        self.key("ctrl+shift+F9")
        wait_for("the fallback shortcut to dismiss the toast",
                 lambda: self.find_node(density_result) is None, timeout=2)
        self.key("ctrl+shift+F8")
        self.find("Mute", "toggle button")
        send_message(alerts, f"<@{owner}> Live update with notifications off")
        self.find("smoke-alerts, 1 unread, 1 pings", "push button")
        self.absent(notification_result, None)
        self.absent(density_result, None)
        self.absent("Open", "push button")

    def check(self, post_live_alert, logs):
        self.find("smoke-target", "push button", partial=True)
        self.check_chat_channel_ellipsis_fill("Smoke Webhook")
        self.click("Smoke Webhook", "push button", partial=True)
        self.find("Smoke deploy alert", partial=True)
        self.visible_text("Smoke embed body")
        self.find("🧪")
        self.find("🔔")
        self.check_webhook_actions(logs)
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
        desktop = Desktop(client, launches, env["PULSE_SERVER"])
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
    portal = Path(env["XDG_CONFIG_HOME"]) / "xdg-desktop-portal"
    portal.mkdir()
    (portal / "portals.conf").write_text("[preferred]\ndefault=gtk\n")
    subprocess.run(
        ["dbus-update-activation-environment", "DISPLAY", "XAUTHORITY", "XDG_RUNTIME_DIR",
         "XDG_CACHE_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CURRENT_DESKTOP"],
        env={**env, "XDG_CURRENT_DESKTOP": "GTK"}, check=True,
    )
    sources = sound_sources(attempt)
    web = web_checkout(work, logs, attempt)
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
        # The upload must fail locally before contacting object storage.
        "NUXT_R2_ACCOUNT_ID": "invalid fixture account",
    })
    subprocess.run(["xset", "b", "off"], check=True)
    subprocess.run(
        ["gdbus", "call", "--session", "--dest", "org.a11y.Bus", "--object-path", "/org/a11y/bus",
         "--method", "org.freedesktop.DBus.Properties.Set", "org.a11y.Status", "IsEnabled", "<true>"],
        check=True, stdout=subprocess.DEVNULL,
    )
    Atspi.init()
    with ((logs / "sound-storage.log").open("w") as storage_log,
          sound_storage(web, attempt / "sound-storage", storage_log) as r2,
          radio_directory(web, attempt),
          livekit_server(attempt, logs / "livekit.log") as (signaling, key, secret),
          silent_audio(attempt, logs / "pulse.log", env)):
        livekit = f"ws://127.0.0.1:{signaling}"
        web_env.update(NUXT_LIVEKIT_API_KEY=key, NUXT_LIVEKIT_API_SECRET=secret,
                       NUXT_PUBLIC_LIVEKIT_URL=livekit)
        web_env.update(r2)
        with service(["node", "node_modules/nuxt/bin/nuxt.mjs", "dev", "--host", "127.0.0.1",
                      "--port", str(port)], server_log, web, web_env) as server:
            post_live_alert, voice, alerts, owner, reader = seed(origin, database, server_log, server)
            binary = desktop_binary(work, logs, {**env, "SPK_URL": origin, "LIVEKIT_URL": livekit})
            with (voice_peer(origin, ADMIN_EMAIL, voice[FIRST_VOICE], web,
                             logs / "author-peer.log", env) as send_message,
                  desktop_session(binary, launches, logs / "reader.log", env) as desktop):
                desktop.sign_in(EMAIL, "Smoke Reader")
                desktop.check_radio_search()
                desktop.check_radio_keyboard(logs)
                desktop.check(post_live_alert, logs)
                desktop.check_reader_menus(logs)
                desktop.check_incoming_toast(logs, send_message, alerts, reader)
                desktop.check_upload_recovery()
                desktop.check_sidebar_resize_retries_save(Path(env["XDG_CONFIG_HOME"]) / "spk-gpui")

            admin_launches = logs / "admin-browser-launches.log"
            admin_launches.touch()
            admin_env = isolated_env(attempt / "admin", admin_launches)
            admin_env["PULSE_SERVER"] = env["PULSE_SERVER"]
            admin = http(origin)
            admin("/api/auth/sign-in/email", {"email": ADMIN_EMAIL, "password": PASSWORD})
            with (voice_peer(origin, EMAIL, voice[FIRST_VOICE], web,
                             logs / "reader-peer.log", env) as send_message,
                  desktop_session(binary, admin_launches, logs / "admin.log", admin_env) as desktop):
                desktop.sign_in(ADMIN_EMAIL)
                desktop.check_soundboard_upload(sources, logs, admin)
                config = Path(admin_env["XDG_CONFIG_HOME"]) / "spk-gpui"
                desktop.check_admin_menus(
                    logs, admin, server,
                    lambda: desktop.check_settings_toasts(logs, config, send_message, alerts, owner),
                )
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
