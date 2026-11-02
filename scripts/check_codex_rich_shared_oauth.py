#!/usr/bin/env python3
"""Exercise shared OAuth with actual official and Rich CLI packages and fake tokens.

Run with CODEX_OFFICIAL_TEST_BIN and CODEX_RICH_TEST_BIN set to the binaries.
File-store checks use temporary configuration, credentials, and loopback HTTP only.
The opt-in Keychain check creates one UUID-named synthetic entry in the real user
Keychain and verifies its removal in registered cleanup. Forced termination can
leave that synthetic entry behind; it never uses an existing server credential.
"""

from contextlib import contextmanager
import http.server
import hashlib
import json
import os
from pathlib import Path
import queue
import re
import signal
import subprocess
import tempfile
import threading
import time
import unittest
import urllib.parse
import uuid


SERVER_NAME = "shared-oauth-contract"
OFFICIAL = os.environ.get("CODEX_OFFICIAL_TEST_BIN")
RICH = os.environ.get("CODEX_RICH_TEST_BIN")


class OAuthFixture:
    def __init__(self):
        self.refresh_started = threading.Event()
        self.release_refresh = threading.Event()
        self.release_refresh.set()
        self.refreshes = []
        self.initialized = []
        self.condition = threading.Condition()
        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def respond(self, status, value=None):
                body = b"" if value is None else json.dumps(value).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self):
                if (
                    "oauth-authorization-server" in self.path
                    or self.path == "/.well-known/openid-configuration"
                ):
                    self.respond(
                        200,
                        {
                            "issuer": fixture.base,
                            "authorization_endpoint": fixture.base + "/authorize",
                            "token_endpoint": fixture.base + "/oauth/token",
                            "registration_endpoint": fixture.base + "/register",
                            "token_endpoint_auth_methods_supported": ["none"],
                            "response_types_supported": ["code"],
                            "code_challenge_methods_supported": ["S256"],
                        },
                    )
                elif "oauth-protected-resource" in self.path:
                    self.respond(
                        200,
                        {
                            "resource": fixture.url,
                            "authorization_servers": [fixture.base],
                        },
                    )
                elif self.path == "/mcp":
                    self.respond(405)
                else:
                    self.respond(404)

            def do_POST(self):
                body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
                if self.path == "/register":
                    registration = json.loads(body)
                    self.respond(
                        201,
                        {
                            "client_id": "synthetic-client",
                            "redirect_uris": registration["redirect_uris"],
                            "token_endpoint_auth_method": "none",
                        },
                    )
                elif self.path == "/oauth/token":
                    form = urllib.parse.parse_qs(body.decode())
                    refresh = form.get("grant_type") == ["refresh_token"]
                    if refresh:
                        with fixture.condition:
                            fixture.refreshes.append(form.get("refresh_token"))
                            fixture.condition.notify_all()
                        fixture.refresh_started.set()
                        if not fixture.release_refresh.wait(45):
                            self.respond(500, {"error": "test_refresh_not_released"})
                            return
                        if form.get("refresh_token") != ["synthetic-refresh"]:
                            self.respond(400, {"error": "invalid_grant"})
                            return
                    self.respond(
                        200,
                        {
                            "access_token": "rotated-access"
                            if refresh
                            else "synthetic-access",
                            "refresh_token": "rotated-refresh"
                            if refresh
                            else "synthetic-refresh",
                            "token_type": "Bearer",
                            "expires_in": 7200,
                        },
                    )
                elif self.path == "/mcp":
                    message = json.loads(body)
                    method = message.get("method")
                    if method == "initialize":
                        with fixture.condition:
                            fixture.initialized.append(
                                self.headers.get("Authorization")
                            )
                            fixture.condition.notify_all()
                        result = {
                            "protocolVersion": message["params"]["protocolVersion"],
                            "capabilities": {"tools": {}},
                            "serverInfo": {"name": "synthetic", "version": "1"},
                        }
                    elif "id" not in message:
                        self.respond(202)
                        return
                    elif method == "tools/list":
                        result = {"tools": []}
                    elif method == "resources/list":
                        result = {"resources": []}
                    elif method == "resources/templates/list":
                        result = {"resourceTemplates": []}
                    elif method == "prompts/list":
                        result = {"prompts": []}
                    else:
                        self.respond(
                            200,
                            {
                                "jsonrpc": "2.0",
                                "id": message["id"],
                                "error": {
                                    "code": -32601,
                                    "message": "unsupported test method",
                                },
                            },
                        )
                        return
                    self.respond(
                        200, {"jsonrpc": "2.0", "id": message["id"], "result": result}
                    )
                else:
                    self.respond(404)

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.base = f"http://127.0.0.1:{self.server.server_port}"
        self.url = self.base + "/mcp"
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def close(self):
        self.release_refresh.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()

    def configure(self, home):
        (home / "config.toml").write_text(f'''mcp_oauth_credentials_store = "file"
model = "synthetic-model"
model_provider = "synthetic"
[model_providers.synthetic]
name = "Synthetic"
base_url = "{self.base}/v1"
env_key = "CODEX_OAUTH_TEST_API_KEY"
wire_api = "responses"
[mcp_servers.{SERVER_NAME}]
url = "{self.url}"
startup_timeout_sec = 30
''')


def environment(home):
    return {
        **os.environ,
        "OPENAI_API_KEY": "synthetic-api-key",
        "CODEX_API_KEY": "synthetic-api-key",
        "CODEX_HOME": str(home),
        "CODEX_OAUTH_TEST_API_KEY": "synthetic-api-key",
        "RUST_LOG": "codex_rmcp_client::oauth::refresh_lock::contention=debug",
    }


@contextmanager
def child(binary, home, *args):
    process = subprocess.Popen(
        [binary, *args],
        cwd=home,
        env=environment(home),
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
        start_new_session=True,
    )
    try:
        yield process
    finally:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=5)
        for stream in [process.stdin, process.stdout, process.stderr]:
            stream.close()


def login(binary, home, server_name=SERVER_NAME, *, file_store=True):
    with child(
        binary,
        home,
        "mcp",
        "login",
        server_name,
        "--no-browser",
        "--oauth-client-registration",
        "dcr",
    ) as process:
        lines = queue.Queue()

        def read(stream):
            for line in stream:
                lines.put(line)

        for stream in [process.stdout, process.stderr]:
            threading.Thread(target=read, args=(stream,), daemon=True).start()
        deadline = time.monotonic() + 30
        while True:
            line = lines.get(timeout=max(0.01, deadline - time.monotonic()))
            match = re.search(r"http://127\.0\.0\.1:[0-9]+/authorize\?\S+", line)
            if match:
                query = urllib.parse.parse_qs(
                    urllib.parse.urlsplit(match.group()).query
                )
                callback = query["redirect_uri"][0]
                separator = "&" if "?" in callback else "?"
                process.stdin.write(
                    callback
                    + separator
                    + urllib.parse.urlencode(
                        {"code": "synthetic-code", "state": query["state"][0]}
                    )
                    + "\n"
                )
                process.stdin.flush()
                break
        if process.wait(timeout=30) != 0:
            raise AssertionError("Synthetic OAuth login failed")
    if file_store:
        return json.loads((home / ".credentials.json").read_text())
    return None


def auth_status(binary, home, server_name=SERVER_NAME):
    result = subprocess.run(
        [binary, "mcp", "list", "--json"],
        cwd=home,
        env=environment(home),
        capture_output=True,
        text=True,
        check=True,
        timeout=30,
    )
    return next(
        entry["auth_status"]
        for entry in json.loads(result.stdout)
        if entry["name"] == server_name
    )


class AppServerProbe:
    def __init__(self, process, home):
        self.process = process
        self.home = str(home)
        self.messages = queue.Queue()
        self.backlog = []
        self.contended = threading.Event()
        self.errors = []

        def output():
            for line in process.stdout:
                self.messages.put(json.loads(line))

        def errors():
            for line in process.stderr:
                self.errors.append(line)
                if (
                    "waiting for another process to finish refreshing MCP OAuth credentials"
                    in line
                ):
                    self.contended.set()

        threading.Thread(target=output, daemon=True).start()
        threading.Thread(target=errors, daemon=True).start()

    def send(self, message):
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()

    def wait(self, predicate):
        for index, message in enumerate(self.backlog):
            if predicate(message):
                return self.backlog.pop(index)
        deadline = time.monotonic() + 35
        while True:
            try:
                message = self.messages.get(
                    timeout=max(0.01, deadline - time.monotonic())
                )
            except queue.Empty as error:
                raise AssertionError(
                    "Synthetic app-server event did not arrive: "
                    + "".join(self.errors[-8:])
                ) from error
            if predicate(message):
                return message
            self.backlog.append(message)

    def start(self):
        self.send(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "clientInfo": {"name": "shared-oauth-probe", "version": "1"},
                    "capabilities": {"experimentalApi": True},
                },
            }
        )
        initialized = self.wait(lambda message: message.get("id") == 1)
        if "error" in initialized:
            raise AssertionError(initialized["error"])
        self.send({"jsonrpc": "2.0", "method": "initialized", "params": {}})
        self.send(
            {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "thread/start",
                "params": {
                    "cwd": self.home,
                    "approvalPolicy": "never",
                    "sandbox": "read-only",
                },
            }
        )

    def ready(self):
        response = self.wait(lambda message: message.get("id") == 2)
        if "error" in response:
            raise AssertionError(response["error"])
        self.wait(
            lambda message: (
                message.get("method") == "mcpServer/startupStatus/updated"
                and message.get("params", {}).get("name") == SERVER_NAME
                and message["params"].get("status") == "ready"
            )
        )


class SharedOAuthTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not OFFICIAL or not RICH:
            raise RuntimeError(
                "Set both CODEX_OFFICIAL_TEST_BIN and CODEX_RICH_TEST_BIN"
            )
        identities = []
        for label, binary, prefix in [
            ("official", OFFICIAL, "codex-cli "),
            ("rich", RICH, "codex-rich "),
        ]:
            path = Path(binary).resolve(strict=True)
            version = subprocess.check_output(
                [str(path), "--version"], text=True, timeout=15
            ).strip()
            if not version.startswith(prefix):
                raise RuntimeError(
                    f"Expected {label} executable version prefix {prefix!r}, got {version!r}"
                )
            with path.open("rb") as source:
                digest = hashlib.file_digest(source, "sha256").hexdigest()
            identities.append(
                {"role": label, "path": str(path), "version": version, "sha256": digest}
            )
        if (
            identities[0]["path"] == identities[1]["path"]
            or identities[0]["sha256"] == identities[1]["sha256"]
        ):
            raise RuntimeError(
                "Official and Rich checks require distinct executable identities"
            )
        print(
            "OAuth binary identities: " + json.dumps(identities, sort_keys=True),
            flush=True,
        )

    def setUp(self):
        self.fixture = OAuthFixture()
        self.addCleanup(self.fixture.close)
        self.temporary = tempfile.TemporaryDirectory(prefix="codex-shared-oauth-")
        self.addCleanup(self.temporary.cleanup)
        self.home = Path(self.temporary.name)
        self.fixture.configure(self.home)

    def test_official_and_rich_write_the_same_credential_key(self):
        official = login(OFFICIAL, self.home)
        self.assertEqual(auth_status(RICH, self.home), "o_auth")
        with tempfile.TemporaryDirectory(prefix="codex-shared-oauth-peer-") as peer:
            home = Path(peer)
            self.fixture.configure(home)
            rich = login(RICH, home)
        self.assertEqual(list(official), list(rich))
        self.assertEqual(len(official), 1)

    def test_shared_logout_preserves_legacy_rich_and_unrelated_entries(self):
        shared = login(OFFICIAL, self.home)
        legacy = self.home / "codex-rich" / ".credentials.json"
        legacy.parent.mkdir()
        legacy.write_text(json.dumps(shared))
        original_legacy = legacy.read_bytes()
        unrelated = dict(next(iter(shared.values())))
        unrelated.update(
            server_name="unrelated", server_url=self.fixture.base + "/other"
        )
        shared["unrelated|fixture"] = unrelated
        (self.home / ".credentials.json").write_text(json.dumps(shared))
        subprocess.run(
            [RICH, "mcp", "logout", SERVER_NAME],
            cwd=self.home,
            env=environment(self.home),
            capture_output=True,
            text=True,
            check=True,
            timeout=30,
        )
        self.assertEqual(legacy.read_bytes(), original_legacy)
        self.assertEqual(
            list(json.loads((self.home / ".credentials.json").read_text())),
            ["unrelated|fixture"],
        )
        self.assertNotEqual(auth_status(OFFICIAL, self.home), "o_auth")

    def test_rotating_refresh_is_shared_across_official_and_rich_processes(self):
        for first, second in [(OFFICIAL, RICH), (RICH, OFFICIAL)]:
            with (
                self.subTest(first=first),
                tempfile.TemporaryDirectory(
                    prefix="codex-oauth-rotation-"
                ) as temporary,
            ):
                home = Path(temporary)
                self.fixture.configure(home)
                stored = login(OFFICIAL, home)
                for entry in stored.values():
                    entry["expires_at"] = 0
                (home / ".credentials.json").write_text(json.dumps(stored))
                self.fixture.refresh_started.clear()
                self.fixture.release_refresh.clear()
                self.fixture.refreshes.clear()
                self.fixture.initialized.clear()
                with child(
                    first, home, "app-server", "--listen", "stdio://"
                ) as first_process:
                    leader = AppServerProbe(first_process, home)
                    leader.start()
                    self.assertTrue(
                        self.fixture.refresh_started.wait(30),
                        "First process must reach provider refresh",
                    )
                    with child(
                        second, home, "app-server", "--listen", "stdio://"
                    ) as second_process:
                        follower = AppServerProbe(second_process, home)
                        follower.start()
                        try:
                            self.assertTrue(
                                follower.contended.wait(25),
                                "Second process must wait on the same credential lock: "
                                + "".join(follower.errors[-8:]),
                            )
                        finally:
                            self.fixture.release_refresh.set()
                        leader.ready()
                        follower.ready()
                self.assertEqual(self.fixture.refreshes, [["synthetic-refresh"]])
                self.assertEqual(
                    self.fixture.initialized,
                    ["Bearer rotated-access", "Bearer rotated-access"],
                )
                updated = json.loads((home / ".credentials.json").read_text())
                self.assertEqual(set(updated), set(stored))
                for entry in updated.values():
                    self.assertEqual(entry["access_token"], "rotated-access")
                    self.assertEqual(entry["refresh_token"], "rotated-refresh")
                self.assertFalse((home / "codex-rich").exists())

    @unittest.skipUnless(
        os.environ.get("CODEX_SHARED_OAUTH_KEYCHAIN_TEST") == "1",
        "Opt in to the synthetic macOS Keychain check after signing the Rich package",
    )
    def test_signed_rich_reads_official_synthetic_keychain_login(self):
        name = "codex-shared-oauth-" + uuid.uuid4().hex
        config = self.home / "config.toml"
        config.write_text(
            config.read_text()
            .replace(
                'mcp_oauth_credentials_store = "file"',
                'mcp_oauth_credentials_store = "keyring"',
            )
            .replace(f"[mcp_servers.{SERVER_NAME}]", f"[mcp_servers.{name}]")
        )

        def cleanup():
            subprocess.run(
                [OFFICIAL, "mcp", "logout", name],
                cwd=self.home,
                env=environment(self.home),
                capture_output=True,
                text=True,
                check=True,
                timeout=30,
            )
            self.assertNotEqual(auth_status(OFFICIAL, self.home, name), "o_auth")

        self.addCleanup(cleanup)
        login(OFFICIAL, self.home, name, file_store=False)
        self.assertFalse(
            (self.home / ".credentials.json").exists(),
            "Keychain check must not silently fall back to File",
        )
        self.assertEqual(auth_status(RICH, self.home, name), "o_auth")
        self.assertFalse((self.home / "codex-rich").exists())


if __name__ == "__main__":
    unittest.main()
