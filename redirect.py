"""mitmproxy addon: send the game's hosts to the local server instead of the official servers.
The original Host header is kept so the server can tell the hosts apart. Everything else passes through.
Run: env -u SSLKEYLOGFILE mitmdump --listen-host 127.0.0.1 -p 8089 --set ssl_insecure=true -s redirect.py
Env: WOOFIA_TARGET (default 127.0.0.1:8720)
"""
import os

TARGET_HOST, _, TARGET_PORT = os.environ.get("WOOFIA_TARGET", "127.0.0.1:8720").partition(":")
TARGET_PORT = int(TARGET_PORT or 8720)

EXACT = {
    "login.playhorny.com",
    "news-service.playhorny.com",
    "proxy.c37e4.com",
    "proxy.canhorny.store",
    "proxy-g.playhorny.com",
    "proxy.bjzyzg.com",
    "recharge.playhorny.com",
}
SUFFIXES = (".woofia.playhorny.com", ".tthrsx.com", ".nexgenph.com", ".bookcr.com")


def ours(host):
    return host in EXACT or host.endswith(SUFFIXES)


def request(flow):
    host = flow.request.pretty_host
    if not ours(host):
        return
    flow.request.scheme = "http"
    flow.request.host = TARGET_HOST
    flow.request.port = TARGET_PORT
    flow.request.host_header = host
