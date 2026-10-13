"""Run the image using synthetic credentials and no live DNS calls."""

import json
import pathlib
import subprocess
import sys
import time
import urllib.error
import urllib.request


def docker(*args):
    return subprocess.check_output(["docker", *args], text=True).strip()


def smoke(mode):
    if mode == "server":
        settings = [
            "-e", "CF_DNS_API_TOKEN=synthetic-smoke-secret",
            "--mount",
            f"type=bind,src={pathlib.Path('config.example.json').resolve()},dst=/config/clients.json,readonly",
        ]
        args = []
        endpoints = [("/v1/challenges", 401), ("/v1/challenges/cleanup", 404), ("/update", 404)]
    else:
        settings = ["-e", "ACME_DNS_SERVER_URL=https://acme.example.com"]
        args = ["--client-mode"]
        endpoints = [("/update", 401), ("/register", 404), ("/v1/challenges", 404)]
    cid = docker("run", "-d", "--read-only", "-p", "127.0.0.1::8080", *settings, sys.argv[1], *args)
    try:
        state = json.loads(docker("inspect", cid))[0]
        port = state["NetworkSettings"]["Ports"]["8080/tcp"][0]["HostPort"]
        base = f"http://127.0.0.1:{port}"
        for attempt in range(30):
            try:
                with urllib.request.urlopen(base + "/healthz", timeout=2) as response:
                    assert response.status == 200
                break
            except (urllib.error.URLError, ConnectionError):
                if attempt == 29:
                    raise
                time.sleep(0.1)
        for endpoint, expected in endpoints:
            request = urllib.request.Request(
                base + endpoint, data=b'{"value":"invalid"}',
                headers={"Content-Type": "application/json"}, method="POST",
            )
            try:
                urllib.request.urlopen(request, timeout=2)
                raise AssertionError("Unexpected successful request")
            except urllib.error.HTTPError as error:
                assert error.code == expected, (error.code, expected)
        assert state["Config"]["User"] == "65532:65532"
        assert state["HostConfig"]["ReadonlyRootfs"]
        docker("stop", "--time", "5", cid)
        state = json.loads(docker("inspect", cid))[0]
        assert state["State"]["ExitCode"] == 0, state["State"]
        logs = subprocess.check_output(["docker", "logs", cid], stderr=subprocess.STDOUT, text=True)
        assert "synthetic-smoke-secret" not in logs
        print(f"Container smoke passed ({mode}): HTTP, authentication, non-root/read-only, SIGTERM.")
    finally:
        docker("rm", "-f", cid)


smoke("server")
smoke("client")
