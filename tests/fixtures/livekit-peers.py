"""Run the controlled peer tests with an isolated local LiveKit server."""

import base64
import hashlib
import hmac
import json
import os
from pathlib import Path
import secrets
import shutil
import socket
import subprocess
import sys
import tarfile
import tempfile
import time
from urllib.request import urlretrieve
import zipfile


RELEASE = "1.13.11"
ARCHIVES = {
    "linux": (
        "linux_amd64.tar.gz",
        "1e9ee90ca301e1842b2887fc77cb122759756a9c8aeb4334ee91de3325ec43fb",
    ),
    "win32": (
        "windows_amd64.zip",
        "5d071ea43339f0140d24a66f0b2ff8a7bf6c4d0dceb19b30abca835670213667",
    ),
}


def server_binary(workspace):
    executable = shutil.which("livekit-server")
    if executable:
        return executable

    if sys.platform not in ARCHIVES:
        raise RuntimeError("Install livekit-server on this platform before running the peer tests")
    suffix, checksum = ARCHIVES[sys.platform]
    name = f"livekit_{RELEASE}_{suffix}"
    archive = workspace / name
    urlretrieve(
        f"https://github.com/livekit/livekit/releases/download/v{RELEASE}/{name}",
        archive,
    )
    if hashlib.sha256(archive.read_bytes()).hexdigest() != checksum:
        raise RuntimeError("LiveKit fixture archive checksum mismatch")

    if sys.platform == "win32":
        binary = workspace / "livekit-server.exe"
        with zipfile.ZipFile(archive) as files, files.open(binary.name) as source:
            with binary.open("wb") as out:
                shutil.copyfileobj(source, out)
    else:
        binary = workspace / "livekit-server"
        with tarfile.open(archive) as files, files.extractfile(binary.name) as source:
            with binary.open("wb") as out:
                shutil.copyfileobj(source, out)
        binary.chmod(0o700)
    return binary


def ports():
    with (
        socket.socket(socket.AF_INET, socket.SOCK_STREAM) as signaling,
        socket.socket(socket.AF_INET, socket.SOCK_STREAM) as tcp,
        socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as udp,
    ):
        for listener in (signaling, tcp, udp):
            listener.bind(("127.0.0.1", 0))
        return tuple(s.getsockname()[1] for s in (signaling, tcp, udp))


def token(key, secret, room, identity):
    def encode(data):
        return base64.urlsafe_b64encode(data).rstrip(b"=")

    header = encode(b'{"alg":"HS256","typ":"JWT"}')
    payload = encode(json.dumps({
        "iss": key,
        "sub": identity,
        "exp": int(time.time()) + 600,
        "video": {"roomJoin": True, "room": room},
    }).encode())
    unsigned = header + b"." + payload
    return (unsigned + b"." + encode(hmac.digest(secret.encode(), unsigned, "sha256"))).decode()


def run_peers(command, signaling, key, secret, room):
    environment = {
        **os.environ,
        "SPK_TEST_LIVEKIT": f"ws://127.0.0.1:{signaling}",
        "SPK_TEST_PUBLISHER": token(key, secret, room, "alice"),
        "SPK_TEST_RECEIVER": token(key, secret, room, "bob"),
    }
    subprocess.run(command, env=environment, check=True, timeout=300)


def compose_server(command):
    docker = shutil.which("docker")
    if not docker:
        raise RuntimeError("Install Docker Compose before running the Compose peer fixture")
    key, secret, room = (secrets.token_hex(16) for _ in range(3))
    compose = [
        docker, "compose", "-f", Path(__file__).with_name("livekit-compose.yaml").resolve(),
        "--project-name", f"spk-livekit-{room}",
    ]
    environment = {**os.environ, "SPK_TEST_LIVEKIT_KEYS": f"{key}: '{secret}'"}
    try:
        subprocess.run(
            [*compose, "up", "--wait", "--wait-timeout", "30"],
            env=environment, check=True, timeout=180,
        )
        run_peers(command, 17880, key, secret, room)
    except subprocess.SubprocessError:
        subprocess.run([*compose, "logs", "--no-color"], env=environment, timeout=15)
        raise
    finally:
        subprocess.run(
            [*compose, "down", "--timeout", "3"], env=environment, check=True, timeout=30,
        )


def native_server(command):
    with tempfile.TemporaryDirectory(prefix="spk-livekit-") as tmp:
        workspace = Path(tmp)
        executable = server_binary(workspace)
        signaling, tcp, udp = ports()
        key, secret, room = (secrets.token_hex(16) for _ in range(3))
        log = workspace / "livekit.log"
        with log.open("wb") as output:
            server = subprocess.Popen(
                [executable, "--bind", "127.0.0.1", "--node-ip", "127.0.0.1",
                 "--udp-port", str(udp), "--keys", f"{key}: {secret}",
                 "--config-body", f"port: {signaling}\nrtc:\n  tcp_port: {tcp}"],
                stdout=output,
                stderr=subprocess.STDOUT,
            )
            try:
                for _ in range(100):
                    if server.poll() is not None:
                        raise RuntimeError("LiveKit fixture exited before accepting connections")
                    try:
                        with socket.create_connection(("127.0.0.1", signaling), timeout=0.1):
                            break
                    except OSError:
                        time.sleep(0.1)
                else:
                    raise RuntimeError("LiveKit fixture did not start")
                run_peers(command, signaling, key, secret, room)
            except (RuntimeError, subprocess.SubprocessError):
                if log.exists():
                    print(log.read_text(errors="replace"), file=sys.stderr)
                raise
            finally:
                server.terminate()
                try:
                    server.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait()


def main(command):
    if os.environ.get("SPK_TEST_LIVEKIT_COMPOSE") == "1":
        compose_server(command)
    else:
        native_server(command)


if __name__ == "__main__":
    main(sys.argv[1:])
