"""Start an isolated native LiveKit server for desktop and controlled peer tests."""

from contextlib import contextmanager
import hashlib
from pathlib import Path
import secrets
import shutil
import socket
import subprocess
import sys
import tarfile
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


@contextmanager
def livekit_server(workspace, log):
    executable = server_binary(workspace)
    key, secret = (secrets.token_hex(16) for _ in range(2))
    attempts = 3
    for attempt in range(attempts):
        signaling, tcp, udp = ports()
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
                        break
                    try:
                        with socket.create_connection(("127.0.0.1", signaling), timeout=0.1):
                            pass
                    except OSError:
                        time.sleep(0.1)
                        continue
                    yield signaling, key, secret
                    return
                else:
                    raise RuntimeError("LiveKit fixture did not start")

                # A port can be claimed between probing it and starting LiveKit.
                if attempt + 1 < attempts and b"address already in use" in log.read_bytes():
                    print("LiveKit fixture port was taken, retrying with new ports", file=sys.stderr)
                    continue
                raise RuntimeError("LiveKit fixture exited before accepting connections")
            except (RuntimeError, subprocess.SubprocessError):
                print(log.read_text(errors="replace"), file=sys.stderr)
                raise
            finally:
                server.terminate()
                try:
                    server.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait()
