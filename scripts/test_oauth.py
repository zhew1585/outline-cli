#!/usr/bin/env python3
"""End-to-end OAuth 2.0 probe for a self-hosted Outline instance.

Validates the exact flow the future Rust CLI will use:
  discovery -> (DCR self-register | provided client) -> authorization code
  + PKCE via loopback redirect -> token exchange -> authenticated API call
  -> refresh grant -> revocation.

Stdlib only. Usage:
  OUTLINE_URL=https://outline.example.com python3 scripts/test_oauth.py [--scope read]
  python3 scripts/test_oauth.py --base https://outline.example.com
  OUTLINE_OAUTH_CLIENT_ID=... [OUTLINE_OAUTH_CLIENT_SECRET=...] to skip DCR.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import http.server
import json
import os
import secrets
import sys
import threading
import urllib.parse
import urllib.request
import webbrowser

CALLBACK_HOST = "127.0.0.1"
CALLBACK_PORT = 8586
CALLBACK_PATH = "/callback"
AUTH_TIMEOUT_SECS = 240


def step(name: str) -> None:
    print(f"\n=== {name} ===")


def fail(msg: str) -> None:
    print(f"FAIL: {msg}")
    sys.exit(1)


def http_json(url: str, data: dict | None = None, headers: dict | None = None,
              form: bool = False) -> tuple[int, dict]:
    body = None
    hdrs = {"Accept": "application/json"}
    if data is not None:
        if form:
            body = urllib.parse.urlencode(data).encode()
            hdrs["Content-Type"] = "application/x-www-form-urlencoded"
        else:
            body = json.dumps(data).encode()
            hdrs["Content-Type"] = "application/json"
    hdrs.update(headers or {})
    req = urllib.request.Request(url, data=body, headers=hdrs)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as e:
        try:
            payload = json.loads(e.read().decode() or "{}")
        except json.JSONDecodeError:
            payload = {}
        return e.code, payload


def discover(base: str) -> dict:
    step("1. Discovery (/.well-known/oauth-authorization-server)")
    status, meta = http_json(f"{base}/.well-known/oauth-authorization-server")
    if status != 200:
        fail(f"metadata endpoint returned {status}")
    for key in ("authorization_endpoint", "token_endpoint"):
        if key not in meta:
            fail(f"metadata missing {key}")
    if "S256" not in meta.get("code_challenge_methods_supported", []):
        fail("server does not advertise PKCE S256")
    print(f"PASS: issuer={meta['issuer']}, PKCE S256 ok, "
          f"scopes={meta.get('scopes_supported')}")
    return meta


def get_client(meta: dict, redirect_uri: str) -> tuple[str, str | None, str]:
    step("2. Client (env or RFC 7591 dynamic registration)")
    env_id = os.environ.get("OUTLINE_OAUTH_CLIENT_ID")
    if env_id:
        print(f"PASS: using client from env: {env_id[:12]}...")
        return env_id, os.environ.get("OUTLINE_OAUTH_CLIENT_SECRET"), "env"
    reg = meta.get("registration_endpoint")
    if not reg:
        fail("no registration_endpoint and no OUTLINE_OAUTH_CLIENT_ID set")
    status, resp = http_json(reg, data={
        "client_name": "outline-cli oauth probe",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    })
    if status not in (200, 201):
        fail(f"DCR failed ({status}): {resp} - if MCP preference is off, "
             "register an app in Settings > Applications and set "
             "OUTLINE_OAUTH_CLIENT_ID")
    print(f"PASS: DCR registered public client {resp['client_id'][:16]}...")
    return resp["client_id"], resp.get("client_secret"), "dcr"


class CallbackHandler(http.server.BaseHTTPRequestHandler):
    result: dict = {}
    event = threading.Event()

    def do_GET(self):  # noqa: N802
        parsed = urllib.parse.urlparse(self.path)
        if parsed.path != CALLBACK_PATH:
            self.send_response(404)
            self.end_headers()
            return
        CallbackHandler.result = dict(urllib.parse.parse_qsl(parsed.query))
        CallbackHandler.event.set()
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.end_headers()
        self.wfile.write("<h2>outline-cli OAuth probe: done, close this tab.</h2>"
                         .encode())

    def log_message(self, *args):  # silence
        pass


def authorize(meta: dict, client_id: str, redirect_uri: str, scope: str) -> tuple[str, str]:
    step("3. Authorization code + PKCE (browser consent)")
    verifier = base64.urlsafe_b64encode(secrets.token_bytes(48)).rstrip(b"=").decode()
    challenge = base64.urlsafe_b64encode(
        hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
    state = secrets.token_urlsafe(16)
    url = meta["authorization_endpoint"] + "?" + urllib.parse.urlencode({
        "response_type": "code",
        "client_id": client_id,
        "redirect_uri": redirect_uri,
        "scope": scope,
        "state": state,
        "code_challenge": challenge,
        "code_challenge_method": "S256",
    })
    server = http.server.HTTPServer((CALLBACK_HOST, CALLBACK_PORT), CallbackHandler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    print(f"Opening browser for consent (waiting up to {AUTH_TIMEOUT_SECS}s)...")
    print(f"If it does not open, visit:\n  {url}")
    webbrowser.open(url)
    ok = CallbackHandler.event.wait(timeout=AUTH_TIMEOUT_SECS)
    server.shutdown()
    if not ok:
        fail("timed out waiting for browser callback")
    result = CallbackHandler.result
    if "error" in result:
        fail(f"authorization denied: {result}")
    if result.get("state") != state:
        fail("state mismatch (possible CSRF)")
    print("PASS: received authorization code, state verified")
    return result["code"], verifier


def exchange(meta: dict, client_id: str, client_secret: str | None,
             code: str, verifier: str, redirect_uri: str) -> dict:
    step("4. Token exchange")
    payload = {
        "grant_type": "authorization_code",
        "code": code,
        "redirect_uri": redirect_uri,
        "client_id": client_id,
        "code_verifier": verifier,
    }
    if client_secret:
        payload["client_secret"] = client_secret
    status, tokens = http_json(meta["token_endpoint"], data=payload, form=True)
    if status != 200 or "access_token" not in tokens:
        fail(f"token exchange failed ({status}): {tokens}")
    print(f"PASS: access_token ({tokens['token_type']}, "
          f"expires_in={tokens.get('expires_in')}s), "
          f"refresh_token={'yes' if tokens.get('refresh_token') else 'no'}")
    return tokens


def call_api(base: str, access_token: str) -> None:
    step("5. Authenticated API call (POST /api/auth.info)")
    status, resp = http_json(f"{base}/api/auth.info", data={}, headers={
        "Authorization": f"Bearer {access_token}"})
    if status != 200:
        fail(f"auth.info returned {status}: {resp}")
    data = resp.get("data", {})
    print(f"PASS: authenticated as {data.get('user', {}).get('name')} "
          f"@ {data.get('team', {}).get('name')}")


def refresh(meta: dict, client_id: str, client_secret: str | None,
            tokens: dict) -> dict:
    step("6. Refresh grant")
    rt = tokens.get("refresh_token")
    if not rt:
        print("SKIP: no refresh_token issued")
        return tokens
    payload = {"grant_type": "refresh_token", "refresh_token": rt,
               "client_id": client_id}
    if client_secret:
        payload["client_secret"] = client_secret
    status, new_tokens = http_json(meta["token_endpoint"], data=payload, form=True)
    if status != 200 or "access_token" not in new_tokens:
        fail(f"refresh failed ({status}): {new_tokens}")
    rotated = new_tokens.get("refresh_token") not in (None, rt)
    print(f"PASS: refreshed, refresh_token rotated={rotated}")
    return new_tokens


def revoke(meta: dict, client_id: str, client_secret: str | None,
           tokens: dict) -> None:
    step("7. Revocation (cleanup)")
    endpoint = meta.get("revocation_endpoint")
    if not endpoint:
        print("SKIP: no revocation_endpoint")
        return
    payload = {"token": tokens["access_token"], "client_id": client_id}
    if client_secret:
        payload["client_secret"] = client_secret
    status, _ = http_json(endpoint, data=payload, form=True)
    print(f"{'PASS' if status == 200 else 'WARN'}: revoke returned {status}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--base",
        default=os.environ.get("OUTLINE_URL"),
        help="Outline instance base URL (defaults to $OUTLINE_URL)",
    )
    parser.add_argument("--scope", default="read")
    args = parser.parse_args()
    if not args.base:
        parser.error("pass --base https://outline.example.com or set OUTLINE_URL")
    base = args.base.rstrip("/")
    redirect_uri = f"http://{CALLBACK_HOST}:{CALLBACK_PORT}{CALLBACK_PATH}"

    meta = discover(base)
    client_id, client_secret, _source = get_client(meta, redirect_uri)
    code, verifier = authorize(meta, client_id, redirect_uri, args.scope)
    tokens = exchange(meta, client_id, client_secret, code, verifier, redirect_uri)
    call_api(base, tokens["access_token"])
    tokens = refresh(meta, client_id, client_secret, tokens)
    call_api(base, tokens["access_token"])
    revoke(meta, client_id, client_secret, tokens)
    print("\nALL CHECKS PASSED - OAuth flow is fully usable for the CLI.")


if __name__ == "__main__":
    main()
