"""Run the controlled peer tests with an isolated local LiveKit server."""

import base64
import hmac
import json
import os
from pathlib import Path
import secrets
import shutil
import subprocess
import sys
import tempfile
import time

from livekit_fixture import livekit_server


def token(key, secret, room, identity, lifetime=600):
    def encode(data):
        return base64.urlsafe_b64encode(data).rstrip(b"=")

    header = encode(b'{"alg":"HS256","typ":"JWT"}')
    payload = encode(json.dumps({
        "iss": key,
        "sub": identity,
        "exp": int(time.time()) + lifetime,
        "video": {"roomJoin": True, "room": room},
    }).encode())
    unsigned = header + b"." + payload
    return (unsigned + b"." + encode(hmac.digest(secret.encode(), unsigned, "sha256"))).decode()


def run_peers(command, signaling, key, secret, room, timeout=300):
    lifetime = max(600, timeout + 60)
    environment = {
        **os.environ,
        "SPK_TEST_LIVEKIT": f"ws://127.0.0.1:{signaling}",
        "SPK_TEST_PUBLISHER": token(key, secret, room, "alice", lifetime),
        "SPK_TEST_HEALTHY_PUBLISHER": token(key, secret, room, "healthy", lifetime),
        "SPK_TEST_MUTED_PUBLISHER": token(key, secret, room, "muted", lifetime),
        "SPK_TEST_RECEIVER": token(key, secret, room, "bob", lifetime),
    }
    subprocess.run(command, env=environment, check=True, timeout=timeout)


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


def native_server(command, timeout=300):
    with tempfile.TemporaryDirectory(prefix="spk-livekit-") as tmp:
        workspace = Path(tmp)
        with livekit_server(workspace, workspace / "livekit.log") as (signaling, key, secret):
            run_peers(command, signaling, key, secret, secrets.token_hex(16), timeout)


def main(command):
    if os.environ.get("SPK_TEST_LIVEKIT_COMPOSE") == "1":
        compose_server(command)
    else:
        native_server(command)


if __name__ == "__main__":
    main(sys.argv[1:])
