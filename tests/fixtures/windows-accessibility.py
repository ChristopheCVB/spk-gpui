"""Exercise the native SPK accessibility tree against disposable loopback servers."""

import base64
from hashlib import sha1
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import mmap
import os
from pathlib import Path
import runpy
from struct import pack
import subprocess
import sys
from tempfile import TemporaryDirectory
from threading import Thread
from urllib.parse import urlsplit
import wave


PROFILE = {"id": "alice", "email": "alice@example.test", "displayName": "Alice", "role": "admin"}
FIXTURE = Path(__file__).resolve()
ROOT = FIXTURE.parents[2]


class SpkFixture(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def respond(self, data, cookie=None):
        body = json.dumps(data).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        if cookie:
            self.send_header("Set-Cookie", cookie)
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length", 0)))
        if self.path == "/api/auth/sign-in/email":
            self.respond({}, "session=fixture; Path=/; HttpOnly; Max-Age=300")
        elif self.path == "/api/voice/token":
            self.respond({
                "token": os.environ["SPK_TEST_PUBLISHER"],
                "url": os.environ["SPK_TEST_LIVEKIT"],
            })
        else:
            self.respond({})

    def do_GET(self):
        path = urlsplit(self.path).path
        if path == "/_ws":
            self.voice_socket()
            return
        self.respond({
            "/api/profile/me": PROFILE,
            "/api/channels": [{"id": "voice", "name": "Fixture voice", "type": "voice"}],
            "/api/voice": [],
            "/api/users": [],
            "/api/sounds": [],
        }.get(path, {}))

    def voice_socket(self):
        nonce = self.headers["Sec-WebSocket-Key"] + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
        self.send_response(101)
        self.send_header("Upgrade", "websocket")
        self.send_header("Connection", "Upgrade")
        self.send_header("Sec-WebSocket-Accept", base64.b64encode(sha1(nonce.encode()).digest()).decode())
        self.end_headers()
        self.close_connection = True

        while len(header := self.rfile.read(2)) == 2:
            opcode = header[0] & 15
            size = header[1] & 127
            if size >= 126:
                size = int.from_bytes(self.rfile.read(2 if size == 126 else 8), "big")
            mask = self.rfile.read(4)
            body = bytes(byte ^ mask[index % 4] for index, byte in enumerate(self.rfile.read(size)))
            if opcode == 8:
                return
            if opcode == 9:
                self.write_frame(10, body)
            if opcode == 1 and (message := json.loads(body)).get("type") == "voice:join":
                joined = {key: message[key] for key in ("sid", "channelId", "nonce") if key in message}
                self.write_frame(1, json.dumps({"type": "voice:joined", **joined}).encode())

    def write_frame(self, opcode, body):
        length = bytes([len(body)]) if len(body) < 126 else b"\x7e" + pack("!H", len(body))
        self.wfile.write(bytes([128 | opcode]) + length + body)


def check():
    with ThreadingHTTPServer(("127.0.0.1", 0), SpkFixture) as server, TemporaryDirectory(
        prefix="spk-a11y-"
    ) as workspace:
        thread = Thread(target=server.serve_forever)
        thread.start()
        try:
            env = {
                **os.environ,
                "SPK_URL": f"http://127.0.0.1:{server.server_port}",
                "LIVEKIT_URL": os.environ["SPK_TEST_LIVEKIT"],
            }
            target = Path(env.get("CARGO_TARGET_DIR", ROOT / "target")).resolve()
            subprocess.run(["cargo", "build", "--locked"], cwd=ROOT, env=env, check=True)
            executable = target / "debug/spk-gpui.exe"
            with executable.open("rb") as file, mmap.mmap(file.fileno(), 0, access=mmap.ACCESS_READ) as binary:
                if binary.find((ROOT / "CHANGELOG.md").read_bytes()) < 0:
                    raise RuntimeError("The Windows executable is missing the bundled changelog")
            sound = Path(workspace) / "fixture.wav"
            with wave.open(str(sound), "wb") as output:
                output.setparams((1, 2, 48000, 0, "NONE", "not compressed"))
                output.writeframes(bytes(96000))
            subprocess.run([
                "powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
                str(FIXTURE.with_suffix(".ps1")), "-Executable", str(executable),
                "-Sound", str(sound),
            ], check=True)
        finally:
            server.shutdown()
            thread.join()


if __name__ == "__main__":
    if sys.platform != "win32":
        sys.exit("Run this fixture from a native Windows session.")
    if sys.argv[1:] == ["--with-livekit"]:
        check()
    else:
        peers = runpy.run_path(str(FIXTURE.with_name("livekit-peers.py")))
        peers["native_server"]([sys.executable, str(FIXTURE), "--with-livekit"], timeout=1200)
