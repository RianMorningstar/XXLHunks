"""End-to-end smoke test against a running server (default http://127.0.0.1:8721), started on an empty data
folder with WOOFIA_TEST_CLOCK=1 (the day checks run its game clock ahead; tests/run_smoke.sh does all this).
Makes its logins on the admin page and plays like the client (PlayHorny login and bind, /acc/auth, /login), then
checks every game call, a second and third player, and the admin page.
Usage: python -I tests/smoke.py [base url]   (needs target/release/decode.exe and the game tables)
"""
import http.client
import json
import urllib.parse
import socket
import struct
import os
import subprocess
import sys
import tempfile
import time
import urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8721"
GAME = "game.woofia.playhorny.com"
DECODE = os.path.join("target", "release", "decode.exe" if os.name == "nt" else "decode")
DATA = os.environ.get("WOOFIA_DATA", os.path.join("target", "devdata"))
LOGIN = "login.playhorny.com"
RAW_TABLES = os.environ.get("WOOFIA_RAW_TABLES", os.path.join("tables", "raw"))
TABLES = os.environ.get("WOOFIA_TABLES", os.path.join("tables", "json"))
fails = 0


def check(name, ok, detail=""):
    global fails
    print(("ok   " if ok else "FAIL ") + name + (f"  ({detail})" if detail else ""))
    if not ok:
        fails += 1


def varint(n):
    out = bytearray()
    while True:
        b = n & 0x7F
        n >>= 7
        if n:
            out.append(b | 0x80)
        else:
            out.append(b)
            return bytes(out)


def field_str(num, s):
    b = s.encode()
    return varint(num << 3 | 2) + varint(len(b)) + b


def field_int(num, v):
    return varint(num << 3) + varint(v)


def call(method, path, body=b"", token=None, host=GAME, extra=None):
    # a kept-alive connection the test closes itself, as the game and mitmproxy do: when the server closes after a
    # large response (urllib's "Connection: close"), Windows sometimes resets the connection before its end arrives
    u = urllib.parse.urlparse(BASE)
    conn = http.client.HTTPConnection(u.hostname, u.port or 80, timeout=30)
    headers = {"Host": host, "Content-Type": "application/x-protobuf"}
    if token:
        headers["Authorization"] = "Bearer " + token
    headers.update(extra or {})
    try:
        conn.request(method, path, body=body if method != "GET" else None, headers=headers)
        r = conn.getresponse()
        return r.status, r.read()
    finally:
        conn.close()


def sse_open(path, token=None):
    """Open an event stream (raw socket: it never ends) and return the socket, the status line and any body read."""
    u = urllib.parse.urlparse(BASE)
    sock = socket.create_connection((u.hostname, u.port or 80), timeout=5)
    auth = f"Authorization: Bearer {token}\r\n" if token else ""
    sock.sendall(f"GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}Accept: text/event-stream\r\n\r\n".encode())
    head = b""
    while b"\r\n\r\n" not in head:
        part = sock.recv(4096)
        if not part:
            break
        head += part
    return sock, head.split(b"\r\n", 1)[0].decode(), head.split(b"\r\n\r\n", 1)[-1]


def sse_next_message(sock, buf=b"", secs=5):
    """The data of the next `message` event on an open stream, as JSON (None if none comes in time)."""
    end = time.time() + secs
    while time.time() < end:
        text = buf.decode("utf-8", "replace")
        i = text.find("event: message\ndata: ")
        if i >= 0 and "\n\n" in text[i + 21:]:
            return json.loads(text[i + 21:].split("\n", 1)[0])
        try:
            part = sock.recv(4096)
        except socket.timeout:
            continue
        if not part:
            break
        buf += part
    return None


def decode_mission_cond(mid):
    """(MissionCondition, ConditionParameter) of a mission, from the decoded Mission_C table."""
    global _MISSIONS
    if "_MISSIONS" not in globals():
        rows = json.load(open(os.path.join(TABLES, "Mission_C.json"), encoding="utf-8"))
        rows = rows if isinstance(rows, list) else rows["rows"]
        _MISSIONS = {r["MissionId"]: (r["MissionCondition"], r["ConditionParameter"]) for r in rows}
    return _MISSIONS.get(mid)


def decode(msg, data):
    with tempfile.NamedTemporaryFile(delete=False, suffix=".bin") as f:
        f.write(data)
    try:
        out = subprocess.run([DECODE, "show", msg, f.name], capture_output=True, text=True, encoding="utf-8", check=True).stdout
        return json.loads(out)
    finally:
        os.unlink(f.name)


def newmask(body):
    # GET g/checknew: field 1 is the red dot mask (varint)
    if body[:1] != b"\x08":
        return 0
    n, shift = 0, 0
    for b in body[1:]:
        n |= (b & 0x7F) << shift
        shift += 7
        if not b & 0x80:
            break
    return n


def qty(items, item_id):
    return sum(s.get("qty", 0) for i in items if i.get("id") == item_id for s in i.get("set", []))


def ph(method, path, body=None, tok=None):
    req = urllib.request.Request(BASE + path, data=json.dumps(body).encode() if body is not None else None, method=method)
    req.add_header("Host", LOGIN)
    req.add_header("Content-Type", "application/json; charset=utf-8")
    if tok:
        req.add_header("Authorization", "Bearer " + tok)
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.status, json.loads(r.read())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"{}")


def ph_login(account, password):
    """PlayHorny login then bind, as the client does: the game id and the bound PlayHorny token."""
    st, r = ph("POST", "/v1/users/login", {"client_id": "84f38c1514", "account": account, "auth_kind": 1, "password": password})
    st, r2 = ph("POST", "/v1/users/game-id/bind", {"client_id": "84f38c1514", "guest_token": ""}, r["data"]["token"])
    return r2["data"]["game_id"], r2["data"]["token"]


def acc_info(game_id, ph_token):
    return field_str(1, f"{game_id}@horny") + field_str(3, ph_token) + field_int(4, 1)


def login(auth_body):
    st, body = call("POST", "/acc/auth", auth_body)
    if st != 200:
        return None, None
    acc = decode("login.AccResponse", body)
    st2, body2 = call("POST", "/login", field_str(1, acc["loginToken"]) + field_int(2, int(acc["id"])))
    resp = decode("login.GameLoginResponse", body2)
    return int(acc["id"]), resp["token"]


# The main test account: a player some days in (level 11, early story cleared, a few dozen buddies and items),
# set up on the admin page.
MAIN_BUDDIES = [  # (id, level, rank, limit break)
    (10000, 1, 0, 0), (10202, 10, 1, 1), (10206, 10, 0, 1), (10207, 1, 0, 0), (10301, 10, 2, 1), (10302, 10, 0, 1),
    (10414, 10, 0, 1), (10010, 1, 0, 0), (10002, 1, 1, 0), (10001, 1, 0, 0), (10008, 1, 0, 0), (10011, 1, 0, 0),
    (10005, 1, 0, 0), (10306, 10, 2, 1), (10009, 1, 0, 0), (10308, 1, 0, 0), (10003, 1, 0, 0), (10013, 1, 0, 0),
    (10404, 15, 0, 2), (10004, 1, 0, 0), (10303, 1, 0, 0), (10211, 1, 0, 0), (10006, 1, 0, 0), (10209, 1, 0, 0),
    (10014, 1, 0, 0), (10210, 1, 0, 0), (10304, 1, 0, 0), (10007, 1, 0, 0), (10307, 10, 2, 1), (10212, 1, 0, 0),
    (10015, 1, 0, 0), (10204, 1, 0, 0), (10201, 1, 0, 0), (10205, 1, 0, 0), (10412, 10, 0, 1),
]
MAIN_ITEMS = [
    (1, 99139), (3, 385), (4, 105), (5, 59798), (8, 165), (9, 250), (10, 6420), (11, 23), (12, 30), (13, 930), (15, 1),
    (16, 2), (19, 240), (10001, 6), (10002, 13), (10003, 12), (10004, 6), (10005, 3), (10008, 3), (10009, 3),
    (10010, 9), (10011, 12), (10013, 9), (10014, 3), (10015, 6), (10207, 5), (10209, 10), (10210, 5), (10308, 10),
    (20001, 8), (21001, 60), (21004, 2), (21013, 62), (30001, 30), (30012, 1000), (31001, 27), (31101, 17),
    (31201, 15), (31301, 12), (32001, 200), (32101, 7), (32102, 3), (32103, 5), (32201, 5), (32202, 1), (32203, 11),
    (32301, 3), (32302, 1), (32303, 3), (32401, 1), (32403, 14), (32501, 3), (32502, 1), (62021, 1), (62061, 1),
    (63011, 1), (63021, 1), (63061, 1), (63071, 1), (64041, 1), (64121, 1), (64141, 1), (70003, 1), (70004, 1),
]
MAIN_CLEARED = [102, 104, 106, 107, 108, 203, 205, 207, 208, 210, 212, 213, 215, 216, 218, 302, 303, 305, 306, 308, 310,
                311, 313, 315, 316, 318, 321, 322, 324]
MAIN_STORY = [101, 103, 105, 109, 201, 202, 204, 206, 209, 211, 214, 217, 219, 220, 221, 301, 304, 307, 309, 312, 314,
              317, 319, 320, 323, 325, 1004401, 1004402]


def setup_main_account(acc_id, token):
    admin = lambda path, form: call("POST", path, form.encode(), host="127.0.0.1")
    admin("/admin/level", f"account={acc_id}&lv=11")
    for cid, lv, rank, lb in MAIN_BUDDIES:
        admin("/admin/char", f"account={acc_id}&id={cid}&lv={lv}&rank={rank}&break={lb}")
    for item, n in MAIN_ITEMS:
        admin("/admin/item", f"account={acc_id}&id={item}&qty={n}")
    for stage in MAIN_CLEARED + MAIN_STORY:
        admin("/admin/story", f"account={acc_id}&stage={stage}")
    team = b"".join(field_int(2, c) for c in (10412, 10414, 10306, 10307, 10302))
    call("POST", "/g/team/battle", field_int(1, 1) + team, token)
    # a few days in: the first (free) rename done, today's sign-ins claimed, Kumoyama in the first hookup room
    call("POST", "/g/userinfo", field_str(1, "WoofMain"), token)
    call("GET", "/g/signin", token=token)
    room = field_int(1, 0) + field_int(2, 10414)
    call("POST", "/g/yiff/bound", varint(1 << 3 | 2) + varint(len(room)) + room, token)


def main():
    sys.stdout.reconfigure(encoding="utf-8")
    call("POST", "/admin/login", b"name=smoke-main@woofia.test&password=smoke-main-pass1", host="127.0.0.1")
    game_id, ph_tok = ph_login("smoke-main@woofia.test", "smoke-main-pass1")
    acc_id, token = login(acc_info(game_id, ph_tok))
    check("login gives a signed server token", token and token.startswith(f"wf.{acc_id}.") and token.count(".") == 3,
          f"account {acc_id}")
    setup_main_account(acc_id, token)

    # a PlayHorny token this server did not sign (as an official-server session would be)
    forged = "eyJhbGciOiJIUzUxMiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiJ4In0.c2lnbmF0dXJl"
    st, _ = call("POST", "/acc/auth", acc_info(game_id, forged))
    check("a PlayHorny token this server did not sign is refused", st == 401, f"status {st}")
    st, _ = call("POST", "/acc/auth", acc_info("someone-else", ph_tok))
    check("a PlayHorny token for another game account is refused", st == 401, f"status {st}")
    st, _ = call("GET", "/g/loginres")
    check("a game call without a token is refused", st == 401, f"status {st}")
    forged = token.rsplit(".", 1)[0].replace(f"wf.{acc_id}.", f"wf.{acc_id + 1}.") + "." + token.rsplit(".", 1)[1]
    st, _ = call("GET", "/g/loginres", token=forged)
    check("a token changed to another account is refused", st == 401, f"status {st}")
    st, _ = call("POST", "/login", field_str(1, f"lt.{acc_id}.00") + field_int(2, acc_id))
    check("an unsigned login token is refused", st == 401, f"status {st}")
    st, _ = ph("GET", "/v1/accounts/game", None, "eyJhbGciOiJIUzUxMiJ9.e30.bad")
    check("an unknown PlayHorny session is not answered from the captures", st == 401, f"status {st}")
    st, _ = call("POST", "/v1/games/accounts/get-points", b"client_id=84f38c1514", host="recharge.playhorny.com")
    check("H Coin needs a PlayHorny token", st == 401, f"status {st}")

    st, body = call("GET", "/g/loginres", token=token)
    snap = decode("save.LoginRes", body)
    items = snap["itemBag"]["items"]
    gems = qty(items, 3)
    check("login snapshot comes from the account", st == 200 and gems > 0, f"gems {gems}")

    # store: first product with stock left in store 1
    store = next(s for s in snap["store"]["stores"] if s["storeId"] == 1)
    prod = next((p for p in store["products"] if p.get("quantity", 0) > 0), None)
    if prod:
        before = qty(items, prod["itemId"])
        st, body = call("POST", "/g/store/buy", field_int(1, 1) + field_int(2, prod.get("productId", 0)) + field_int(3, 1),
                        token)
        check("store buy answers", st == 200, f"status {st}")
        if st == 200:
            r = decode("store.StoreBuyResponse", body)
            after = qty(r.get("finalItems", []), prod["itemId"])
            check("store buy adds the item", after > before, f"item {prod['itemId']}: {before} -> {after}")
            st, body = call("POST", "/g/store/buy",
                            field_int(1, 1) + field_int(2, prod.get("productId", 0)) + field_int(3, prod["quantity"] + 1), token)
            check("store refuses more than the stock left", st == 400, f"status {st}")

    st, body = call("POST", "/g/mall/ingame/purchase", field_int(1, 22041), token)
    check("mall product 22041 (free pack) answers", st == 200, f"status {st}")
    st, body = call("GET", "/g/checknew", token=token)
    check("mail red dot shows while a mail waits", st == 200 and body[:1] == b"\x08" and body[1] & 1, body.hex())
    st, body = call("GET", "/g/mailbox/0", token=token)
    mails = decode("mail.MailBox", body).get("mails", [])
    pack = [m for m in mails if m.get("mailText", {}).get("pra") == ["22041"] and not m.get("flag", 0) & 2]
    check("the pack arrives as an unclaimed mail", len(pack) >= 1, f"{len(mails)} mails")
    if pack:
        m = pack[-1]
        rid = m["rewards"][0]["id"]
        before = qty(decode("save.LoginRes", call("GET", "/g/loginres", token=token)[1])["itemBag"]["items"], rid)
        st, body = call("POST", "/g/mailbox/mail/rewards", field_str(1, m["sn"]), token)
        r = decode("mail.GetMailRewardResponse", body)
        check("claiming adds the rewards", r.get("validSn") == [m["sn"]] and qty(r.get("finalItems", []), rid) > before,
              f"item {rid}: {before} -> {qty(r.get('finalItems', []), rid)}")
        st, body = call("POST", "/g/mailbox/mail/rewards", field_str(1, m["sn"]), token)
        r2 = decode("mail.GetMailRewardResponse", body)
        check("claiming the same mail again gives nothing", not r2.get("validSn") and not r2.get("gainRewards"))
        all_sn = [x["sn"] for x in decode("mail.MailBox", call("GET", "/g/mailbox/0", token=token)[1]).get("mails", [])]
        call("POST", "/g/mailbox/mail/read", b"".join(field_str(1, x) for x in all_sn), token)
        call("POST", "/g/mailbox/mail/rewards", b"".join(field_str(1, x) for x in all_sn), token)
        st, body = call("GET", "/g/checknew", token=token)
        check("mail red dot clears once everything is read and claimed", st == 200 and not (body[:1] == b"\x08" and body[1] & 1),
              body.hex())
        st, body = call("POST", "/g/mailbox/mail/remove", field_str(2, m["sn"]), token)
        st, body = call("GET", "/g/mailbox/0", token=token)
        check("removed mail is gone", all(x["sn"] != m["sn"] for x in decode("mail.MailBox", body).get("mails", [])))
        # a notice from the admin page (no items) can be deleted; a mail with rewards waiting cannot
        call("POST", "/admin/notice", f"account={acc_id}&subject=Smoke notice&content=Hello".encode(), host="127.0.0.1")
        call("POST", "/admin/mail", f"account={acc_id}&id=1&qty=5".encode(), host="127.0.0.1")
        box = decode("mail.MailBox", call("GET", "/g/mailbox/0", token=token)[1]).get("mails", [])
        notice = [x for x in box if "Smoke notice" in x.get("mailText", {}).get("subject", "")]
        gift = [x for x in box if x.get("rewards") and not x.get("flag", 0) & 2]
        check("an admin gift without a product id is a titled mail (no product text)", any(
            "Gift" in x.get("mailText", {}).get("subject", "") and not x.get("mailText", {}).get("pra") for x in gift), f"{gift}")
        sns = [x["sn"] for x in notice + gift]
        call("POST", "/g/mailbox/mail/remove", b"".join(field_str(2, x) for x in sns), token)
        left = {x["sn"] for x in decode("mail.MailBox", call("GET", "/g/mailbox/0", token=token)[1]).get("mails", [])}
        check("a notice without items is deleted; a mail with rewards waiting stays", len(notice) == 1 and gift
              and notice[0]["sn"] not in left and all(x["sn"] in left for x in gift), f"{len(notice)} notices, {len(gift)} gifts")
        # an item capped at 1 (maxCount, item 2001): the second copy's mail cannot be claimed and stays in the box
        have = qty(decode("save.LoginRes", call("GET", "/g/loginres", token=token)[1])["itemBag"]["items"], 2001)
        for _ in range(2 - have):
            call("POST", "/admin/mail", f"account={acc_id}&id=2001&qty=1".encode(), host="127.0.0.1")
        box = decode("mail.MailBox", call("GET", "/g/mailbox/0", token=token)[1]).get("mails", [])
        capped = [x["sn"] for x in box if any(int(i["id"]) == 2001 for i in x.get("rewards", [])) and not x.get("flag", 0) & 2]
        st, body = call("POST", "/g/mailbox/mail/rewards", b"".join(field_str(1, x) for x in capped), token)
        r = decode("mail.GetMailRewardResponse", body) if st == 200 else {}
        left = {x["sn"] for x in decode("mail.MailBox", call("GET", "/g/mailbox/0", token=token)[1]).get("mails", [])}
        check("a mail that would take an item past its cap stays unclaimed (the client's 23016)", len(capped) == 2 - have
              and len(r.get("validSn", [])) == 1 - have and qty(r.get("finalItems", []), 2001) in (0, 1) and capped[-1] in left,
              f"had {have}, mails {len(capped)}, {r}")

    energy = qty(items, 4)
    potions = qty(items, 21013)
    if potions > 0:
        st, body = call("POST", "/g/stage/buy/energy", field_int(1, 21013) + field_int(2, potions) + field_int(3, 1), token)
        r = decode("stage.BuyEnergyResponse", body) if st == 200 else {}
        new_energy = qty(r.get("lastItem", []), 4)
        check("energy item gives 10 energy", new_energy == energy + 10, f"{energy} -> {new_energy}")

    st, body = call("GET", "/g/loginres", token=token)
    snap2 = decode("save.LoginRes", body)
    check("changes are saved", qty(snap2["itemBag"]["items"], 4) == (energy + 10 if potions else energy))

    def snapshot_rooms():
        return decode("save.LoginRes", call("GET", "/g/loginres", token=token)[1]).get("yiff", {}).get("bound", [])

    draws = []
    for _ in range(2):
        st, body = call("POST", "/g/yiff/femdom/start", field_int(1, 0), token)
        draws.append(decode("yiff.FemdomStartResponse", body).get("drawCard", []) if st == 200 else [])
    check("hookup draws 9 different cards", len(draws[0]) == 9 and len(set(draws[0])) == 9, str(draws[0]))
    check("hookup draws change between starts", draws[0] != draws[1])
    def bound(idx, cid):
        nb = field_int(1, idx) + field_int(2, cid)
        return call("POST", "/g/yiff/bound", varint(1 << 3 | 2) + varint(len(nb)) + nb, token)
    st, _ = bound(2, 10414)
    check("the third room needs the monthly card with that perk", st == 400, f"status {st}")
    lv0 = decode("save.LoginRes", call("GET", "/g/loginres", token=token)[1]).get("basic", {}).get("lv", 1)
    call("POST", "/admin/level", f"account={acc_id}&lv=12".encode(), host="127.0.0.1")
    st, _ = bound(1, 10414)
    check("the second room opens at level 13", st == 400, f"status {st}")
    call("POST", "/admin/level", f"account={acc_id}&lv={max(lv0, 13)}".encode(), host="127.0.0.1")
    login0 = decode("save.LoginRes", call("GET", "/g/loginres", token=token)[1])
    xl_less = next((c["id"] for c in login0["charBag"]["chars"] if c["id"] // 100 in (100, 102)), 0)
    in_rooms = {x.get("charId") for x in login0.get("yiff", {}).get("bound", [])}
    free_xl = [c["id"] for c in login0["charBag"]["chars"] if c["id"] // 100 in (103, 104) and c["id"] not in in_rooms]
    st, _ = bound(1, xl_less)
    check("an M or L buddy cannot go in a room", st == 400, f"{xl_less}: status {st}")
    st, b = bound(1, free_xl[0])
    rooms = decode("yiff.NewBoundResponse", b).get("bound", []) if st == 200 else []
    check("a character goes into room 1", any(x.get("idx") == 1 and x.get("charId") == free_xl[0] for x in rooms), f"status {st}")
    st, _ = bound(1, free_xl[1])
    check("a working room cannot take another buddy", st == 400, f"status {st}")
    st, b = call("PUT", "/g/yiff/bound", field_str(1, "\x01"), token)
    r = decode("yiff.BoundReceiveResponse", b) if st == 200 else {}
    check("collecting a fresh room gives no EXP yet", st == 200 and r.get("reports") and not r["reports"][0].get("exp"), f"{r}")
    st, b = call("DELETE", "/g/yiff/bound", field_str(1, "\x01"), token)
    r = decode("yiff.BoundReceiveResponse", b) if st == 200 else {}
    room1 = next((x for x in snapshot_rooms() if x.get("idx") == 1), {})
    rep = (r.get("reports") or [{}])[0]
    check("ending a room empties it, its last time (the client's end time) now, and starts its cooldown", st == 200
          and r.get("remove") and int(room1.get("endTime", 0)) > 0 and not room1.get("charId")
          and abs(int(rep.get("lastTime", 0)) - int(room1.get("endTime", 0))) <= 1, f"{r}, room {room1}")
    st, _ = bound(1, free_xl[1])
    check("a cleaning room cannot take a buddy", st == 400, f"status {st}")
    time.sleep(16)
    room1 = next((x for x in snapshot_rooms() if x.get("idx") == 1), {})
    check("after its 15 second cooldown the room is empty again", not room1.get("charId") and int(room1.get("endTime", 0)) == 0, f"room {room1}")
    st, _ = call("POST", "/g/yiff/femdom/start", field_int(1, 0) + field_int(2, 1), token)
    check("hookup skips need the bond outfit first (FinishYiff)", st == 400, f"status {st}")
    picks = [80120, 80126, 80127]  # Kumoyama's three perfect cards
    st, body = call("POST", "/g/yiff/femdom/finish", field_int(1, 0) + b"".join(field_int(2, c) for c in picks), token)
    r = decode("yiff.FemdomResponse", body) if st == 200 else {}
    check("three right cards give femdom_exp_bonus3 (8) x 3600 bond EXP", r.get("exp") == 28800, f"exp {r.get('exp')}")

    def snapshot():
        return decode("save.LoginRes", call("GET", "/g/loginres", token=token)[1])

    def gacha(gid, times, use_ticket=True):
        body = field_int(1, gid) + field_int(2, times) + field_int(3, 1 if use_ticket else 0)
        st, b = call("POST", "/g/gacha", body, token)
        return st, (decode("gacha.GachaResponse", b) if st == 200 else {})

    before = snapshot()
    gems0 = qty(before["itemBag"]["items"], 3)
    st, r = gacha(1, 10, use_ticket=False)
    after = snapshot()
    check("gacha 10-pull gives 11 results", st == 200 and len(r.get("rewards", [])) == 11, f"status {st}")
    check("gacha 10-pull with gems costs 200", qty(after["itemBag"]["items"], 3) == gems0 - 200,
          f"{gems0} -> {qty(after['itemBag']['items'], 3)}")
    new_ids = [x["param"][0] for x in r.get("rewards", []) if len(x.get("param", [])) == 2]
    have = {c["id"] for c in after["charBag"]["chars"]}
    check("new characters join the account", all(i in have for i in new_ids), f"new {new_ids}")
    fresh = [c for c in after["charBag"]["chars"] if c["id"] in new_ids and c["id"] not in {x["id"] for x in before["charBag"]["chars"]}]
    check("a new buddy has its first story reward waiting (flag 64)", all(c.get("flag", 0) & 64 for c in fresh),
          f"{[(c['id'], c.get('flag')) for c in fresh]}")

    t2 = qty(after["itemBag"]["items"], 20002)
    bp = qty(after["itemBag"]["items"], 11)
    st, r = gacha(48, 1)
    after2 = snapshot()
    check("pickup banner single pull uses one ticket and gives 1 Bonus Pt",
          st == 200 and qty(after2["itemBag"]["items"], 20002) == t2 - 1 and qty(after2["itemBag"]["items"], 11) == bp + 1,
          f"tickets {t2}->{qty(after2['itemBag']['items'], 20002)}, bonus {bp}->{qty(after2['itemBag']['items'], 11)}")

    st, r = gacha(12, 1, use_ticket=False)
    recs = snapshot()["mall"]["gacha"]["gachaRecs"]
    check("a banner's first pull makes a record with a wish list (the client needs it to log in)",
          all("wishList" in x for x in recs) and any(x["id"] == 12 for x in recs), f"status {st}")
    wish = [10405, 10411, 10419, 10406, 10403]
    packed = b"".join(varint(x) for x in wish)
    st, b = call("PUT", "/g/gacha/wishlist/12", varint(1 << 3 | 2) + varint(len(packed)) + packed, token)
    rec12 = next(x for x in snapshot()["mall"]["gacha"]["gachaRecs"] if x["id"] == 12)
    check("the standard banner's wish list is saved", st == 200 and rec12.get("wishList", {}).get("contentsId") == wish, f"status {st}")
    st, b = call("PUT", "/g/gacha/wishlist/12", varint(1 << 3 | 2) + varint(len(varint(10001))) + varint(10001), token)
    check("a non-XXL cannot be wished for", st == 400, f"status {st}")
    wish0 = [10405, 10411, 0, 0, 0]
    packed = b"".join(varint(x) for x in wish0)
    st, b = call("PUT", "/g/gacha/wishlist/12", varint(1 << 3 | 2) + varint(len(packed)) + packed, token)
    rec12 = next(x for x in snapshot()["mall"]["gacha"]["gachaRecs"] if x["id"] == 12)
    check("a wish list with empty slots (0) is saved as sent", st == 200 and rec12.get("wishList", {}).get("contentsId") == wish0,
          f"status {st}, {rec12.get('wishList')}")
    # the standard banner's daily free pull starts over each game day
    free = lambda: call("POST", "/g/gacha", field_int(1, 12) + field_int(2, 1) + field_int(5, 1), token)[0]
    st, st2 = free(), free()
    call("POST", "/admin/clock", b"secs=86400", host="127.0.0.1")
    st3 = free()
    call("POST", "/admin/clock", b"reset=1", host="127.0.0.1")
    check("one free pull a day on the standard banner, again the next day", st == 200 and st2 == 400 and st3 == 200,
          f"{st}, {st2}, {st3}")
    s0 = snapshot()
    st, b = call("POST", "/g/gacha/buy_ticket", field_int(1, 1) + field_int(2, 3), token)
    r = decode("gacha.GachaBuyTicketResponse", b) if st == 200 else {}
    check("buying 3 Silver Bells costs 60 gems",
          st == 200 and qty([r["ticket"]], 20001) == qty(s0["itemBag"]["items"], 20001) + 3
          and qty(r.get("last", []), 3) == qty(s0["itemBag"]["items"], 3) - 60
          and qty(r.get("last", []), 20001) == qty(s0["itemBag"]["items"], 20001) + 3,
          f"status {st}, {r}")
    st, b = call("POST", "/g/gacha/buy_ticket", field_int(1, 48) + field_int(2, 1), token)
    check("Golden Bells (no gem price) cannot be bought with gems", st == 400, f"status {st}")
    def pick(cid):
        st, b = call("POST", "/g/gacha/exchange", field_int(1, 1) + varint(2 << 3 | 2) + varint(len(field_int(1, 1) + field_int(2, cid)))
                     + field_int(1, 1) + field_int(2, cid), token)
        return st, (decode("gacha.GachaExchangeResponse", b) if st == 200 else {})

    st, r = pick(10404)
    count1 = next(x["count"] for x in snapshot()["mall"]["gacha"]["gachaRecs"] if x["id"] == 1)
    check("buddy pick waits for 88 summons", (st == 400) == (count1 < 88), f"count {count1}, status {st}")
    call("POST", "/admin/item", f"account={acc_id}&id=3&qty=100000".encode(), host="127.0.0.1")
    while count1 < 88:
        gacha(1, 10, use_ticket=False)
        count1 = next(x["count"] for x in snapshot()["mall"]["gacha"]["gachaRecs"] if x["id"] == 1)
    st, r = pick(10001)
    check("buddy pick refuses a non-SSR", st == 400, f"status {st}")
    st, r = pick(10416)
    check("buddy pick gives the chosen SSR", st == 200 and r["reward"]["param"][0] == 10416, f"status {st}")
    st, r = pick(10405)
    check("buddy pick works only once", st == 400, f"status {st}")
    call("POST", "/admin/item", f"account={acc_id}&id=20002&qty=1000".encode(), host="127.0.0.1")
    ssr, total, streak, longest = 0, 0, 0, 0
    for _ in range(30):
        st, r = gacha(48, 10)
        for x in r.get("rewards", []):
            total += 1
            if x["param"][0] // 100 == 104:
                ssr += 1
                streak = 0
            else:
                streak += 1
                longest = max(longest, streak)
    check("SSR rate is near 3% and no streak passes the 80-pull pity", total == 330 and 0 < ssr and longest < 80,
          f"{ssr} SSR in {total} results, longest streak {longest}")

    call("POST", "/admin/login", b"name=smoke-new@woofia.test&password=smoke-new-pass1", host="127.0.0.1")
    new_game_id, new_ph_tok = ph_login("smoke-new@woofia.test", "smoke-new-pass1")
    new_auth = acc_info(new_game_id, new_ph_tok)
    new_id, new_token = login(new_auth)
    check("a new account name gets its own account", new_id != acc_id, f"{new_id}")
    st, body = call("GET", "/g/loginres", token=new_token)
    fresh = decode("save.LoginRes", body)
    check("new account starts from the fresh snapshot", int(fresh["basic"]["id"]) == new_id,
          f"lv {fresh['basic'].get('lv')}, gems {qty(fresh['itemBag']['items'], 3)}")
    again_id, _ = login(new_auth)
    check("same name logs back into the same account", again_id == new_id)

    # a new account starts fresh: every running sign-in gives its day 1 now, with the rewards listed
    st, body = call("GET", "/g/signin", token=new_token)
    datas = decode("signin.SignInResponse", body).get("datas", []) if st == 200 else []
    check("a new account's sign-ins all give day 1 (rewards listed), none copied as claimed", datas
          and all(d.get("isSignIn", [])[:1] == [True] and d.get("items") for d in datas), f"{[(d['id'], d.get('items')) for d in datas]}")
    lr = decode("save.LoginRes", call("GET", "/g/loginres", token=new_token)[1])
    check("after the day's sign-ins the login says none is waiting (no empty popup)", not lr.get("canGetSignInReward"),
          f"{lr.get('canGetSignInReward')}")
    wp = next((d for d in datas if d["id"] == 1001), {})
    check("Welcome Party on its first day lists one day (no later day shown as missed)", wp.get("isSignIn") == [True], f"{wp}")
    st, body = call("GET", "/g/mailbox/0", token=new_token)
    bc = [m for m in decode("mail.MailBox", body).get("mails", []) if m.get("flag", 0) & 8 and m.get("rewards")] if st == 200 else []
    if bc:
        st, body = call("POST", "/g/mailbox/mail/rewards", field_str(1, bc[0]["sn"]), new_token)
        rec = decode("mail.GetMailRewardResponse", body).get("recordedMails", []) if st == 200 else []
        st2, body2 = call("GET", "/g/mailbox/0", token=new_token)
        sns = [m["sn"] for m in decode("mail.MailBox", body2).get("mails", [])]
        check("a claimed broadcast mail comes back as the player's copy under a new sn", len(rec) == 1
              and rec[0]["sn"] != bc[0]["sn"] and rec[0]["sn"] in sns and bc[0]["sn"] not in sns, f"{[m['sn'] for m in rec]}")
    else:
        check("a new account has the broadcast mails", False, "none")

    # guilds: shared by the server's accounts
    def gsave_basic(tok):
        st, body = call("GET", "/g/loginres", token=tok)
        return decode("save.LoginRes", body).get("basic", {}) if st == 200 else {}

    def gsave(tok):
        st, body = call("GET", "/g/guild/save/0", token=tok)
        return decode("guild.GuildSaveResponse", body) if st == 200 else {}
    check("no guild to start with", int(gsave(token).get("guild", {}).get("id", 0)) == 0)
    icon = field_int(1, 2) + field_int(2, 3)
    new_guild = field_str(1, "WoofGuild") + field_str(2, "hi") + varint(3 << 3 | 2) + varint(len(icon)) + icon
    st, _ = call("POST", "/g/guild/new", new_guild, token)
    g = gsave(token)
    gid = int(g.get("guild", {}).get("id", 0))
    check("a guild is created with its maker as leader and checked in once",
          st == 200 and gid and int(g["guild"]["leaderId"]) == acc_id and qty(g.get("signinReward", []), 13) == 50, f"status {st}, {g.get('signinReward')}")
    check("checking in again the same day gives nothing", not gsave(token).get("signinReward"))
    st, body = call("POST", "/g/guild/search", field_str(2, "woof"), new_token)
    found = decode("guild.GuildSearchResponse", body).get("infos", []) if st == 200 else []
    check("another account finds the guild by name", any(int(x["id"]) == gid for x in found), f"{found}")
    st, body = call("POST", "/g/guild/member/apply", field_int(1, gid), new_token)
    r = decode("guild.GuildMemberApplyResponse", body) if st == 200 else {}
    st2, body = call("GET", "/g/guild/member/list", token=token)
    members = [int(m["id"]) for m in decode("guild.GuildMemberListResponse", body).get("members", [])]
    check("joining a guild without approval is immediate (code 8)", r.get("code") == 8 and new_id in members, f"{r}, {members}")
    call("POST", "/g/guild/post", field_str(1, "hello guild") + field_int(2, 0), token)
    new_post = decode("guild.GuildCheckNewPostResponse", call("GET", "/g/guild/post/checknew", token=new_token)[1]).get("code")
    st, body = call("POST", "/g/guild/post/info", field_int(1, 0), new_token)
    posts = decode("guild.GuildAskPostInfoResponse", body).get("posts", [])
    read = decode("guild.GuildCheckNewPostResponse", call("GET", "/g/guild/post/checknew", token=new_token)[1]).get("code")
    check("the board's new-post flag is 1 for an unread post, 2 once read", new_post == 1 and read == 2, f"{new_post}, {read}")
    check("guild chat reaches the other member", any(p["content"] == "hello guild" and int(p["userId"]) == acc_id for p in posts), f"{posts}")
    st, body = call("POST", "/g/userinfo/basic", field_int(1, acc_id) + field_int(1, new_id), token)
    names = {int(i["userId"]): i.get("name") for i in decode("userinfo.UserBasicInfoResponse", body).get("infos", [])}
    check("member names come from this server's accounts", acc_id in names and new_id in names, f"{names}")
    wish_char = next(c["id"] for c in decode("save.LoginRes", call("GET", "/g/loginres", token=new_token)[1])["charBag"]["chars"]
                     if c["id"] > 10000)
    st, _ = call("POST", "/g/guild/wish/post", field_int(1, wish_char), new_token)
    check("a member makes a wish for an owned buddy's shards", st == 200, f"status {st}")
    st, body = call("DELETE", "/g/guild/wish", b"", new_token)
    st2, body2 = call("GET", "/g/guild/wish", token=token)
    mine = next((w for w in decode("guild.GuildWishInfoResponse", body2).get("infos", []) if int(w["memberId"]) == new_id), {})
    check("a wish with no stars can be removed (code 1), and members without a wish are not listed",
          decode("guild.GuildCancelWishResponse", body).get("code") == 1 and not mine, f"status {st}, {mine}")
    st, _ = call("POST", "/g/guild/wish/post", field_int(1, 30004), new_token)
    check("a wish can be for universal XXL shards", st == 200, f"status {st}")
    for star in (14, 15):
        call("POST", "/admin/item", f"account={acc_id}&id={star}&qty=10".encode(), host="127.0.0.1")
    donate = field_int(1, new_id) + varint(2 << 3 | 2) + varint(2) + varint(1) + varint(1)
    st, body = call("POST", "/g/guild/wish/donate", donate, token)
    r = decode("guild.GuildDonateWishResponse", body) if st == 200 else {}
    check("stars can go to a wish the day you join (captured), 5 Guild Pt each", r.get("code") == 1
          and qty([r.get("getItems", {})], 13) == 10 and r.get("wishInfo", {}).get("todayDonatedCount") == 2, f"{r}")
    big = field_int(1, new_id) + varint(2 << 3 | 2) + varint(2) + varint(5) + varint(0)
    st, body = call("POST", "/g/guild/wish/donate", big, token)
    check("a wish takes wish_star_donate_limit (6) stars a day (code 4)", decode("guild.GuildDonateWishResponse", body).get("code") == 4,
          f"status {st}")
    st, body = call("DELETE", "/g/guild/wish", b"", new_token)
    check("a wish with stars cannot be cancelled (code 3)", decode("guild.GuildCancelWishResponse", body).get("code") == 3, f"status {st}")
    mask = newmask(call("GET", "/g/checknew", token=new_token)[1])
    check("an unfinished wish lights no guild red dot", not mask >> 3 & 1, f"{mask:b}")
    # Escape Room: the event cycle, floor 0 with the entry open, revealing next to it
    info = gsave(token).get("guild", {}).get("dungeonInfo", {})
    check("the guild carries the Escape Room cycle from dungeon_event_C", int(info.get("eventId", 0)) > 0
          and info.get("mapIds") == [111, 112, 113] and int(info.get("stateEndUnix", 0)) > 0, f"{info}")
    st, body = call("GET", "/g/guild/dungeon/floor/0", token=token)
    fl = decode("guildbattle.GuildDungeonFloor", body) if st == 200 else {}
    grid = fl.get("floor", {})
    cells = grid.get("cells", [])
    open_cells = [c for c in cells if c.get("objectId", 0) != -1]
    check("floor 0 is map 111 (15 x 10), every room sent, only the entry open", st == 200 and grid.get("mapId") == 111
          and grid.get("row") == 15 and grid.get("col") == 10 and len(cells) == 150 and len(open_cells) == 1,
          f"status {st}, {grid.get('mapId')}, {len(cells)} cells, {len(open_cells)} open")
    ex, ey = (open_cells[0].get("x", 0), open_cells[0].get("y", 0)) if open_cells else (5, 14)
    call("POST", "/admin/item", f"account={acc_id}&id=29&qty=100".encode(), host="127.0.0.1")
    reveal = field_int(1, ex) + field_int(2, ey - 1) + field_int(3, 0) + field_int(4, 0)
    _, nostatus, _ = sse_open("/sse/guild/dungeon/0")
    check("the Escape Room stream needs a token", " 401" in nostatus, nostatus)
    stream_sock, sstatus, sbuf = sse_open("/sse/guild/dungeon/0", new_token)
    check("a guild member opens the Escape Room stream for floor 0", " 200" in sstatus, sstatus)
    st, body = call("POST", "/g/guild/dungeon/cell/reveal", reveal, token)
    r = decode("guildbattle.GuildDungeonRevealCellResponse", body) if st == 200 else {}
    live = sse_next_message(stream_sock, sbuf)
    stream_sock.close()
    check("the reveal reaches the other member's stream as JSON (cells, sender)", live is not None
          and int(live.get("senderId", 0)) == acc_id and len(live.get("cells", [])) == 1 and "finalItems" not in live, f"{live}")
    check("revealing the room above the entry costs 5 Escape Energy and gives 5 Guild Pt", st == 200 and len(r.get("cells", [])) == 1
          and qty(r.get("finalItems", []), 29) == 95 and qty(r.get("reward", []), 13) == 5, f"status {st}, {r}")
    st, body = call("POST", "/g/guild/dungeon/cell/reveal", field_int(1, 0) + field_int(2, 0) + field_int(3, 0), token)
    check("a room that is not next to an open one cannot be revealed (code 2)", st == 200
          and decode("guildbattle.GuildDungeonRevealCellResponse", body).get("code") == 2, f"status {st}")
    st, _ = call("GET", "/g/guild/dungeon/floor/2", token=token)
    check("floors above the Exploration level are closed", st == 400, f"status {st}")
    st, body = call("GET", "/g/guild/dungeon/log/0", token=token)
    logs = decode("guildbattle.GuildDungeonLog", body).get("logs", []) if st == 200 else []
    check("Found logs name the object found (the client reads args[0] as one)",
          all((l.get("args") or [0])[0] > 0 for l in logs if l.get("type") == 3), f"{logs}")
    for path in ("/g/guild/dungeon/sync", "/g/guild/dungeon/rewards"):
        st, _ = call("GET", path, token=token)
        check(f"{path} answers", st == 200, f"status {st}")
    buy = field_int(1, 3) + field_int(3, 1)
    st, body = call("POST", "/g/guild/dungeon/buy/energy", buy, token)
    r = decode("stage.BuyEnergyResponse", body) if st == 200 else {}
    check("buying Escape Energy with gems gives 50", st == 200 and qty(r.get("lastItem", []), 29) == 145 and r.get("purchaseTimes") == 1, f"status {st}, {r}")
    # Escape Room rooms: reveal outward until a room that takes points, then contribute to it. A room next to a live
    # monster stays closed, as in the client, and the 20 monsters of map 111 can wall the walk in (about 1 floor in 7),
    # so when no room can be revealed the walk beats a monster it has found. Every room but NoGround is reachable from
    # the entry (lay_out), so the walk always ends on a room that takes points.
    ob = open(os.path.join(RAW_TABLES, "dungeon_obj_C.bytes"), "rb").read()
    dkinds = {struct.unpack_from("<5i", ob, 4 + 20 * i)[0]: struct.unpack_from("<5i", ob, 4 + 20 * i)[1:]
              for i in range(struct.unpack_from("<i", ob)[0])}
    def dkind(c):
        return dkinds.get(c.get("objectId", 0), (0,))[0]
    def floor0():
        st, body = call("GET", "/g/guild/dungeon/floor/0", token=token)
        return {(c.get("x", 0), c.get("y", 0)): c for c in decode("guildbattle.GuildDungeonFloor", body)["floor"]["cells"]}
    def live_monster(c):
        return dkind(c) == 2 and not (c.get("args") or [0])[0]
    def reveal_any(cells):
        # the client's rule: a hidden room next to an open one that is not NoGround (kind 11)
        frontier = [(x + dx, y + dy) for (x, y), c in cells.items() if c.get("objectId", 0) != -1 and dkind(c) != 11
                    for dx, dy in ((1, 0), (-1, 0), (0, 1), (0, -1)) if cells.get((x + dx, y + dy), {}).get("objectId") == -1]
        for fx, fy in dict.fromkeys(frontier):
            st, body = call("POST", "/g/guild/dungeon/cell/reveal", field_int(1, fx) + field_int(2, fy) + field_int(4, 0), token)
            if st == 200 and not decode("guildbattle.GuildDungeonRevealCellResponse", body).get("code"):
                return True
        return False
    def battle_over(sn):
        res = field_int(1, 0) + field_int(5, 3) + field_int(8, 5000)
        return field_str(1, str(sn)) + varint(3 << 3 | 2) + varint(len(res)) + res
    def beat_a_monster(cells):
        xy = next((xy for xy, c in cells.items() if live_monster(c)), None)
        if xy is None:
            return False
        st, body = call("POST", "/g/guild/dungeon/cell/", field_int(1, xy[0]) + field_int(2, xy[1]), token)
        sn = (decode("guildbattle.GuildDungeonCellResponse", body).get("args") or [0])[0] if st == 200 else 0
        st, _ = call("POST", "/g/guild/dungeon/battle/finish", battle_over(sn), token) if sn else (0, b"")
        return st == 200
    call("POST", "/admin/item", f"account={acc_id}&id=29&qty=2000".encode(), host="127.0.0.1")
    target, walk = None, []
    for _ in range(200):
        cells = floor0()
        target = next((xy for xy, c in cells.items() if dkind(c) in (3, 4, 9, 10) and not c.get("args", [0])[0]), None)
        if target:
            break
        if reveal_any(cells):
            walk.append("r")
        elif beat_a_monster(cells):
            walk.append("f")
        else:
            break
    check("revealing outward finds a room that takes points", target is not None,
          f"{walk.count('r')} reveals, {walk.count('f')} fights, energy {qty(snapshot()['itemBag']['items'], 29)}")
    if target:
        need = dkinds[cells[target]["objectId"]][3]
        energy = qty(snapshot()["itemBag"]["items"], 29)
        args = varint(4 << 3 | 2) + varint(2) + varint(1) + varint(35)
        st, body = call("POST", "/g/guild/dungeon/cell/", field_int(1, target[0]) + field_int(2, target[1]) + args, token)
        r = decode("guildbattle.GuildDungeonCellResponse", body) if st == 200 else {}
        check("contributing 1 point takes 1 Escape Energy and shows the progress", st == 200 and r.get("cells", [{}])[0].get("args") == [1]
              and qty(r.get("finalItems", []), 29) == energy - 1, f"status {st}, {r.get('cells')}")
        rest = need - 1
        args = varint(4 << 3 | 2) + varint(len(varint(rest)) + 1) + varint(rest) + varint(1)
        st, body = call("POST", "/g/guild/dungeon/cell/", field_int(1, target[0]) + field_int(2, target[1]) + args, token)
        r = decode("guildbattle.GuildDungeonCellResponse", body) if st == 200 else {}
        st2, body2 = call("POST", "/g/guild/dungeon/cell/", field_int(1, target[0]) + field_int(2, target[1]) + varint(4 << 3 | 2) + varint(2) + varint(1) + varint(1), token)
        r2 = decode("guildbattle.GuildDungeonCellResponse", body2) if st2 == 200 else {}
        after = (r2.get("cells") or [{}])[0].get("args")
        # ([0, 0]: the chest was a Mimic, now an enemy; [need, 1] an opened chest; code 2 refused)
        check(f"a room filled to {need} points takes no more", st == 200 and (r2.get("code") == 2 or after in ([need], [need, 1], None, [], [0, 0])),
              f"status {st}, then {st2} {after}, {r.get('reward')}")
    # Escape Room fights: reveal outward until a monster, start the fight, then win it
    mon = None
    for _ in range(200):
        cells = floor0()
        mon = next((xy for xy, c in cells.items() if live_monster(c)), None)
        if mon or not reveal_any(cells):
            break
    check("revealing outward finds a monster", mon is not None)
    if mon:
        cost = dkinds[cells[mon]["objectId"]][3]
        energy = qty(snapshot()["itemBag"]["items"], 29)
        st, body = call("POST", "/g/guild/dungeon/cell/", field_int(1, mon[0]) + field_int(2, mon[1]), token)
        r = decode("guildbattle.GuildDungeonCellResponse", body) if st == 200 else {}
        sn = (r.get("args") or [0])[0]
        check(f"tapping a monster starts a fight for {cost} Escape Energy", st == 200 and sn > 0 and qty(r.get("finalItems", []), 29) == energy - cost,
              f"status {st}, {r}")
        over = battle_over(sn)
        st, body = call("POST", "/g/guild/dungeon/battle/finish", over, token)
        r = decode("guildbattle.GuildDungeonCellResponse", body) if st == 200 else {}
        st2, body = call("GET", "/g/guild/dungeon/floor/0", token=token)
        after = {(c.get("x", 0), c.get("y", 0)): c for c in decode("guildbattle.GuildDungeonFloor", body)["floor"]["cells"]}
        check("a beaten monster keeps its room, read as beaten (args[0] = 1)", st == 200
              and after[mon].get("objectId") == cells[mon].get("objectId") and (after[mon].get("args") or [0])[0] == 1,
              f"status {st}, {r}, {after[mon]}")
        st, body = call("POST", "/g/guild/dungeon/battle/finish", over, token)
        check("a fight cannot be finished twice (code 3)", st == 200
              and decode("guildbattle.GuildDungeonCellResponse", body).get("code") == 3, f"status {st}")
    # Escape Room Tallying: end the run from the admin page; participants still in the guild get the rewards by mail
    st, body = call("GET", "/g/guild/dungeon/rewards", token=token)
    gathered = decode("guildbattle.GuildDungeonRewards", body).get("rewards", []) if st == 200 else []
    call("POST", "/admin/escape-end", f"account={acc_id}&end=1".encode(), host="127.0.0.1")
    def escape_mails(tok):
        st, body = call("GET", "/g/mailbox/0", token=tok)
        return [m for m in decode("mail.MailBox", body).get("mails", [])
                if "Escape Room Reward" in m.get("mailText", {}).get("subject", "")] if st == 200 else []
    mine = escape_mails(token)
    check("at Tallying a participant gets the Escape Room rewards by mail",
          len(mine) == 1 and sorted((r["id"], r["set"][0]["qty"]) for r in mine[0].get("rewards", []))
          == sorted((r["id"], r["set"][0]["qty"]) for r in gathered) and gathered, f"{gathered}, {mine}")
    check("a member who did not take part gets no mail", escape_mails(new_token) == [])
    check("the rewards are mailed only once", len(escape_mails(token)) == 1)
    st, body = call("GET", "/g/guild/dungeon/stats", token=token)
    r = decode("guildbattle.GuildDungeonStatsResponse", body) if st == 200 else {}
    check("the Tallying screen counts the run", st == 200 and r.get("participantCount") == 1 and r.get("hasGainReward")
          and r.get("mobKilled", 0) >= (1 if mon else 0), f"status {st}, {r}")
    st, body = call("GET", "/g/guild/save/0", token=token)
    info = decode("guild.GuildSaveResponse", body).get("guild", {}).get("dungeonInfo", {}) if st == 200 else {}
    st2, body2 = call("POST", "/g/guild/dungeon/cell/reveal", field_int(1, 0) + field_int(2, 0) + field_int(4, 0), token)
    check("an ended run shows Tallying and takes no more actions", info.get("state") == 2
          and decode("guildbattle.GuildDungeonRevealCellResponse", body2).get("code") == 2, f"{info}, {st2}")
    call("POST", "/admin/escape-end", f"account={acc_id}&end=0".encode(), host="127.0.0.1")
    st, body = call("GET", "/g/guild/save/0", token=token)
    info = decode("guild.GuildSaveResponse", body).get("guild", {}).get("dungeonInfo", {}) if st == 200 else {}
    check("the admin page can open the run again", info.get("state", 0) == 1, f"{info}")
    st, body = call("POST", "/g/guild/dungeon/boss/simulate", field_int(1, 0) + field_int(2, 0) + field_int(3, 0), token)
    r = decode("guildbattle.GuildDungeonBossSimulate", body) if st == 200 else {}
    check("the boss fight preview answers the floor's altar skills and the damage so far (code 0)", st == 200
          and r.get("code", 0) == 0, f"status {st}, {r}")
    # held in Preparing from the admin page: Wishing Stars go to the floors (3 a day), then back to the calendar
    call("POST", "/admin/escape-end", f"account={acc_id}&end=prepare".encode(), host="127.0.0.1")
    joined_day = gsave(token).get("guild", {}).get("dungeonInfo", {}).get("canDonateStar")
    call("POST", "/admin/clock", b"secs=86400", host="127.0.0.1")
    for item in (14, 15, 16):
        call("POST", "/admin/item", f"account={acc_id}&id={item}&qty=10".encode(), host="127.0.0.1")
    info = gsave(token).get("guild", {}).get("dungeonInfo", {})
    packed = varint(1) + varint(1) + varint(1)
    st, body = call("POST", "/g/guild/dungeon/star", varint(1 << 3 | 2) + varint(len(packed)) + packed, token)
    r = decode("guildbattle.GuildDungeonStarResponse", body) if st == 200 else {}
    st2, body2 = call("POST", "/g/guild/dungeon/star", varint(1 << 3 | 2) + varint(len(packed)) + packed, token)
    check("held in Preparing: no stars on the joining day; the next day 3 can be donated, then refused (code 2)", info.get("state", 0) == 0
          and info.get("canDonateStar") and st == 200 and not r.get("code") and r.get("starCounts", [])[:3] == [1, 1, 1]
          and decode("guildbattle.GuildDungeonStarResponse", body2).get("code") == 2 and not joined_day, f"{info}, {r}")
    call("POST", "/admin/clock", b"reset=1", host="127.0.0.1")
    call("POST", "/admin/escape-end", f"account={acc_id}&end=0".encode(), host="127.0.0.1")
    info = gsave(token).get("guild", {}).get("dungeonInfo", {})
    check("opened again, the run follows the calendar (Active)", info.get("state", 0) == 1, f"{info}")
    st, _ = call("POST", "/g/guild/new", field_str(1, "12345"), new_token)
    check("a guild name of only numbers is refused", st == 400, f"status {st}")
    st, body = call("POST", "/g/guild/member/adjust", field_int(1, new_id) + field_int(2, 2), token)
    st2, body2 = call("POST", "/g/guild/member/leave", b"", token)
    check("the leader can make a vice leader (code 0) but cannot leave while others are in (code 2)",
          decode("guild.GuildMemberAdjustResponse", body).get("code", 0) == 0 and st == 200
          and decode("guild.GuildMemberLeaveResponse", body2).get("code") == 2, f"{st}, {st2}")
    # a leader away for 7 days hands the guild to the vice leader on another member's next guild call
    call("POST", "/admin/leader-away", f"account={acc_id}".encode(), host="127.0.0.1")
    st, body = call("GET", "/g/guild/save/0", token=new_token)
    g = decode("guild.GuildSaveResponse", body).get("guild", {}) if st == 200 else {}
    afk = [l for l in g.get("logs", []) if l.get("kind") == 7]
    titles = {int(m["id"]): m.get("title") for m in g.get("members", [])}
    check("an absent leader's guild passes to the vice leader (log kind 7: old, new)", int(g.get("leaderId", 0)) == new_id
          and titles.get(new_id) == 1 and titles.get(acc_id) == 10 and afk and [int(x) for x in afk[-1].get("pra", [])] == [acc_id, new_id],
          f"leader {g.get('leaderId')}, titles {titles}, logs {afk}")
    call("GET", "/g/guild/save/0", token=token)
    st, _ = call("POST", "/g/guild/member/adjust", field_int(1, acc_id) + field_int(2, 1), new_token)
    st2, body = call("GET", "/g/guild/save/0", token=token)
    check("the old leader, back again, can be handed the guild back", st == 200
          and int(decode("guild.GuildSaveResponse", body).get("guild", {}).get("leaderId", 0)) == acc_id, f"status {st}")
    def energy_of(tok):
        items = decode("save.LoginRes", call("GET", "/g/loginres", token=tok)[1]).get("itemBag", {}).get("items", [])
        return qty(items, 29)

    call("GET", "/g/guild/dungeon/sync", token=new_token)
    energy_before = energy_of(new_token)
    st, _ = call("POST", "/g/guild/member/leave", b"", new_token)
    check("leaving a guild resets Escape Energy to 0 (the client's leave prompt says so)",
          energy_before > 0 and energy_of(new_token) == 0, f"before {energy_before}, after {energy_of(new_token)}")
    st2, body = call("GET", "/g/guild/member/list", token=token)
    members = [int(m["id"]) for m in decode("guild.GuildMemberListResponse", body).get("members", [])]
    st3, body3 = call("POST", "/g/guild/member/apply", field_int(1, gid), new_token)
    check("a member can leave, and must wait before joining again (code 7)", st == 200 and members == [acc_id]
          and decode("guild.GuildMemberApplyResponse", body3).get("code") == 7, f"{st}, {members}, {st3}")
    call("POST", "/admin/rejoin-now", f"account={new_id}".encode(), host="127.0.0.1")
    st, body = call("POST", "/g/guild/member/apply", field_int(1, gid), new_token)
    check("the admin page clears the wait: the player joins again (code 8)", decode("guild.GuildMemberApplyResponse", body).get("code") == 8,
          f"status {st}")
    call("POST", "/g/guild/member/leave", b"", new_token)
    st, _ = call("POST", "/g/store/buy", field_int(1, 3) + field_int(2, 0) + field_int(3, 1), new_token)
    check("the guild shop needs a guild", st == 400, f"status {st}")
    setting = field_str(1, "hi") + varint(2 << 3 | 2) + varint(len(icon)) + icon + field_int(3, 1)
    call("POST", "/g/guild/setting", setting, token)
    call("POST", "/admin/login", b"name=smoke-third@woofia.test&password=smoke-third-pass1", host="127.0.0.1")
    third_id, third_token = login(acc_info(*ph_login("smoke-third@woofia.test", "smoke-third-pass1")))
    st, body = call("POST", "/g/guild/member/apply", field_int(1, gid), third_token)
    r = decode("guild.GuildMemberApplyResponse", body) if st == 200 else {}
    applicants = [int(x) for x in gsave(token).get("guild", {}).get("tbd", [])]
    call("POST", "/g/guild/member/approve", field_int(1, third_id), token)
    st2, body = call("GET", "/g/guild/member/list", token=token)
    members = [int(m["id"]) for m in decode("guild.GuildMemberListResponse", body).get("members", [])]
    check("with approval on, an application waits (code 0) and the leader approves it", r.get("code", 0) == 0 and st == 200
          and third_id in applicants and third_id in members,
          f"{r}, {applicants}, {members}")
    call("POST", "/admin/rejoin-now", f"account={new_id}".encode(), host="127.0.0.1")
    call("POST", "/g/guild/member/apply", field_int(1, gid), new_token)
    st, body = call("GET", "/g/guild/applieds", token=new_token)
    applied = [int(x["id"]) for x in decode("guild.GuildAppliedsResponse", body).get("infos", [])] if st == 200 else []
    check("a waiting application is listed for the applicant", gid in applied, f"status {st}, {applied}")
    st, body = call("POST", "/g/guild/member/cancel", field_int(1, gid), new_token)
    r = decode("guild.GuildMemberCancelResponse", body) if st == 200 else {}
    tbd = [int(x) for x in gsave(token).get("guild", {}).get("tbd", [])]
    check("the applicant can cancel (code 0, with the guild's name)", st == 200 and r.get("code", 0) == 0
          and r.get("guildName") and new_id not in tbd, f"{r}, {tbd}")
    call("POST", "/g/guild/member/apply", field_int(1, gid), new_token)
    st, body = call("POST", "/g/guild/member/reject", field_int(1, new_id), token)
    r = decode("guild.GuildMemberRejectResponse", body) if st == 200 else {}
    tbd = [int(x) for x in gsave(token).get("guild", {}).get("tbd", [])]
    check("the leader can reject an application (code 0)", st == 200 and r.get("code", 0) == 0
          and [int(x) for x in r.get("rejectedIds", [])] == [new_id] and new_id not in tbd, f"{r}, {tbd}")
    st, body = call("POST", "/g/guild/member/remove", field_int(1, third_id), token)
    st2, body2 = call("GET", "/g/guild/member/list", token=token)
    members = [int(m["id"]) for m in decode("guild.GuildMemberListResponse", body2).get("members", [])]
    check("the leader can remove a member (code 0)", st == 200 and decode("guild.GuildMemberRemoveResponse", body).get("code", 0) == 0
          and third_id not in members, f"status {st}, {members}")
    call("POST", "/admin/item", f"account={acc_id}&id=21012&qty=1".encode(), host="127.0.0.1")
    st, body = call("POST", "/g/guild/name", field_str(1, "WoofGuild2"), token)
    r = decode("guild.GuildSettingNameResponse", body) if st == 200 else {}
    check("the leader renames the guild for a Guild Name Ticket (code 0)", st == 200 and r.get("code", 0) == 0
          and gsave(token).get("guild", {}).get("name") == "WoofGuild2", f"status {st}, {r}")
    mascots = [m["RoleId"] for m in json.load(open(os.path.join(TABLES, "GuildMascot_C.json"), encoding="utf-8"))["rows"] if m["Lv"] <= 1]
    st, body = call("POST", "/g/guild/mascot", field_int(1, mascots[-1]), token)
    check("the leader picks a mascot open at the guild's level (code 1)", st == 200
          and decode("guild.GuildSettingMascotResponse", body).get("code") == 1
          and int(gsave(token).get("guild", {}).get("mascotId", 0)) == mascots[-1], f"status {st}")
    st, body = call("GET", "/g/guild/recommend", token=new_token)
    recs = [int(x["id"]) for x in decode("guild.GuildSearchResponse", body).get("infos", [])] if st == 200 else []
    check("the guild list for a player outside a guild shows the open guild", gid in recs, f"status {st}, {recs}")
    st, body = call("GET", "/g/guild/wish/log", token=token)
    check("the wish donation log answers (code 1)", st == 200 and decode("guild.GuildDonateLogInfoResponse", body).get("code") == 1,
          f"status {st}")
    st, body = call("GET", "/g/guild/wish/receive", token=token)
    check("a wish that is not full cannot be received", st == 400, f"status {st}")

    st, body = call("GET", "/admin", host="127.0.0.1")
    check("admin page lists both accounts", st == 200 and str(acc_id).encode() in body and str(new_id).encode() in body)
    st, body = call("POST", "/admin/item", f"account={new_id}&id=3&qty=12345".encode(), host="127.0.0.1")
    st, body = call("GET", "/g/loginres", token=new_token)
    check("admin sets an item on the chosen account", qty(decode("save.LoginRes", body)["itemBag"]["items"], 3) == 12345)

    call("POST", "/admin/hcoin", f"account={acc_id}&qty=500".encode(), host="127.0.0.1")
    st, body = call("POST", "/v1/games/accounts/get-points", b"client_id=84f38c1514", ph_tok, host="recharge.playhorny.com")
    check("H Coin balance from the admin page", st == 200 and json.loads(body)["data"]["points"] == 500, body[:80])
    st, body = call("GET", "/v1/users/hcoin", token=ph_tok, host="recharge.playhorny.com")
    check("H Coin total on the PlayHorny page call", st == 200 and json.loads(body)["data"]["h_coins"]["total"] == 500)
    st0, _ = call("POST", "/g/mall/cash/purchase", field_int(2, 20101), token)
    check("a pack on an ended mall page (201) cannot be bought", st0 == 400, f"status {st0}")
    st, body = call("POST", "/g/mall/cash/purchase", field_int(2, 20301), token)
    st2, body2 = call("POST", "/v1/games/accounts/get-points", b"", ph_tok, host="recharge.playhorny.com")
    mails = decode("mail.MailBox", call("GET", "/g/mailbox/0", token=token)[1]).get("mails", [])
    check("H Coin pack 20301 costs 240 and arrives by mail",
          st == 200 and json.loads(body2)["data"]["points"] == 260 and any(m.get("mailText", {}).get("pra") == ["20301"] for m in mails),
          f"status {st}")
    call("POST", "/admin/hcoin", f"account={acc_id}&qty=5000".encode(), host="127.0.0.1")
    st, body = call("POST", "/g/mall/cash/purchase", field_int(2, 42106), token)
    mails = decode("mail.MailBox", call("GET", "/g/mailbox/0", token=token)[1]).get("mails", [])
    pack = next((m for m in mails if m.get("mailText", {}).get("pra") == ["42106"]), {})
    ids = sorted(r["id"] for r in pack.get("rewards", []))
    check("a box pack arrives opened: its six items, not the box", ids == [1, 5, 10444, 20002, 21003, 30012], f"{ids}")
    call("POST", "/admin/item", f"account={acc_id}&id=40126&qty=2".encode(), host="127.0.0.1")
    use = field_int(1, 40126) + varint(2 << 3 | 2) + varint(len(field_int(1, 2))) + field_int(1, 2)
    st, body = call("POST", "/g/item/consume", varint(1 << 3 | 2) + varint(len(use)) + use, token)
    r = decode("item.ConsumeItemResponse", body) if st == 200 else {}
    got = {i["id"]: i["set"][0]["qty"] for i in r.get("reward", [{}])[0].get("items", [])} if r else {}
    check("opening 2 all-in crates (kind 20) from the bag gives every entry of each", set(got) == {21014, 21015}
          and all(v >= 2 for v in got.values()), f"status {st}, {got}")
    call("POST", "/admin/item", f"account={acc_id}&id=40014&qty=1".encode(), host="127.0.0.1")
    use = field_int(1, 40014) + varint(2 << 3 | 2) + varint(len(field_int(1, 1))) + field_int(1, 1)
    st, _ = call("POST", "/g/item/consume", varint(1 << 3 | 2) + varint(len(use)) + use, token)
    check("a pack box (kind 23, opened on receipt) is not used from the bag", st == 400, f"status {st}")
    call("POST", "/admin/item", f"account={acc_id}&id=42009&qty=1".encode(), host="127.0.0.1")
    use = field_int(1, 42009) + varint(2 << 3 | 2) + varint(len(field_int(1, 1))) + field_int(1, 1)
    st, body = call("POST", "/g/item/consume", varint(1 << 3 | 2) + varint(len(use)) + use, token)
    r = decode("item.ConsumeItemResponse", body) if st == 200 else {}
    got = r.get("reward", [{}])[0].get("items", []) if r else []
    check("a random crate (Lucky Treat XL) gives exactly one of its entries", st == 200 and len(got) == 1,
          f"status {st}, {got}")
    gems_before = qty(decode("save.LoginRes", call("GET", "/g/loginres", token=token)[1])["itemBag"]["items"], 3)
    st, body = call("POST", "/g/mall/cash/purchase", field_int(2, 42001), token)
    check("First Encounter (sign-in pass) can be bought", st == 200, f"status {st}")
    st, body = call("GET", "/g/signin", token=token)
    r = decode("signin.SignInResponse", body) if st == 200 else {}
    pass101 = next((d for d in r.get("datas", []) if d["id"] == 101), {})
    check("First Encounter day 1 is claimed on sign-in, its rewards listed for the popup", pass101.get("isSignIn", [])[:1] == [True]
          and qty(r.get("finalItems", []), 3) == gems_before + 30 and qty(pass101.get("items", []), 3) == 30, f"{pass101}")
    st, body = call("GET", "/g/signin", token=token)
    r2 = decode("signin.SignInResponse", body) if st == 200 else {}
    check("signing in again the same day gives nothing", not r2.get("finalItems"))
    st, body = call("POST", "/g/mall/cash/purchase", field_int(2, 99999999), token)
    check("an unknown product is refused before charging", st == 400, f"status {st}")
    call("POST", "/admin/hcoin", f"account={acc_id}&qty=100".encode(), host="127.0.0.1")
    st, body = call("POST", "/g/mall/cash/purchase", field_int(2, 20103), token)
    check("H Coin pack refused without enough H Coin", st == 400, f"status {st}")
    call("POST", "/admin/hcoin", f"account={acc_id}&qty=5000".encode(), host="127.0.0.1")
    for start, finish, retry in (("/g/mall/cash/purchase/mega", "/g/mall/cash/purchase/mega/finish", "/g/mall/cash/purchase/mega/retry"),
                                 ("/g/dmm/receipt/payment", "/g/dmm/receipt/payment/finish", "/g/dmm/receipt/payment/retry")):
        call("POST", "/admin/packs-period-end", f"account={acc_id}&merch=1301".encode(), host="127.0.0.1")
        st, body = call("POST", start, field_int(2, 20301), token)
        r = decode("mall.CashPurchaseResponse", body) if st == 200 else {}
        st2, body2 = call("POST", finish, b"", token)
        r2 = decode("mall.CashPurchaseResponse", body2) if st2 == 200 else {}
        st3, _ = call("POST", retry, b"", token)
        check(f"{start}: paid (status 4 with a transaction id), finished (channel 2), retry answers", r.get("status") == 4
              and r.get("transactionId") and r2.get("transactionId") == r.get("transactionId") and r2.get("channelStatus") == 2
              and st3 == 200, f"{st} {r}, {st2} {r2}, {st3}")
    call("POST", "/admin/packs-period-end", f"account={acc_id}&merch=1301".encode(), host="127.0.0.1")
    st, body = call("POST", "/g/dmm/payment", field_int(1, 20301), token)
    r = decode("dmm.DmmCheckPaymentResponse", body) if st == 200 else {}
    st2, body2 = call("POST", "/g/dmm/payment/reward", b"", token)
    r2 = decode("mall.CashPurchaseResponse", body2) if st2 == 200 else {}
    check("DMM item payment: status 0 with its key, then the reward call finishes it", r.get("status") == "0"
          and r.get("k") and r2.get("transactionId") == r.get("k"), f"{r}, {r2}")

    # PlayHorny login service: a second player with their own login and account
    st, body = call("POST", "/admin/login", b"name=player2@woofia.local&password=smoke-test-pass1", host="127.0.0.1")
    check("admin refuses a login name the app cannot type in", b"woofia.test" in body, f"status {st}")
    pw = "smoke-test-pass1"
    call("POST", "/admin/login", f"name=player2@woofia.test&password={pw}".encode(), host="127.0.0.1")

    st, r = ph("POST", "/v1/users/login", {"client_id": "84f38c1514", "account": "player2@woofia.test", "auth_kind": 1, "password": "wrong-pass"})
    check("PlayHorny login refuses a wrong password", st == 400, f"status {st}")
    st, r = ph("POST", "/v1/users/login", {"client_id": "84f38c1514", "account": "player2@woofia.test", "auth_kind": 1, "password": pw})
    check("PlayHorny login accepts the right password", st == 200 and r["data"]["token"].count(".") == 2, f"status {st}")
    st, r2 = ph("POST", "/v1/users/game-id/bind", {"client_id": "84f38c1514", "guest_token": ""}, r["data"]["token"])
    game_id = r2.get("data", {}).get("game_id", "")
    st, r3 = ph("GET", "/v1/accounts/game", None, r2["data"]["token"])
    check("bind and account lookup give the same game id", game_id and r3["data"]["account"] == game_id, game_id)
    st, r4 = ph("POST", "/v1/users/refresh/token", {"client_id": "84f38c1514"}, r2["data"]["token"])
    st2, r5 = ph("GET", "/v1/users/info", None, r2["data"]["token"])
    check("PlayHorny token refresh and user info answer for the logged-in player", st == 200
          and r4["data"]["game_id"] == game_id and r4["data"]["token"].count(".") == 2 and st2 == 200
          and r5["data"]["email"] == "player2@woofia.test", f"{st} {r4}, {st2}")
    p2_id, p2_token = login(acc_info(game_id, r2["data"]["token"]))
    check("the second player gets their own game account", p2_id not in (acc_id,), f"{p2_id}")
    call("POST", "/admin/hcoin", f"account={p2_id}&qty=77".encode(), host="127.0.0.1")
    req = urllib.request.Request(BASE + "/v1/games/accounts/get-points", data=b"client_id=84f38c1514", method="POST")
    req.add_header("Host", "recharge.playhorny.com")
    req.add_header("Authorization", "Bearer " + r2["data"]["token"])
    with urllib.request.urlopen(req, timeout=30) as resp:
        pts = json.loads(resp.read())["data"]["points"]
    check("H Coin follows the logged-in player", pts == 77, f"points {pts}")

    # character progression: costs from CharacterLvUp_C, LvBreak_C, CharacterStar_C and SkillLv_C
    lvup = {r["Lv"]: r for r in json.load(open(os.path.join(TABLES, "CharacterLvUp_C.json"), encoding="utf-8"))["rows"]}

    def char(cid):
        return next(c for c in snapshot()["charBag"]["chars"] if c["id"] == cid)

    def give(item, n):
        call("POST", "/admin/item", f"account={acc_id}&id={item}&qty={n}".encode(), host="127.0.0.1")

    cid = next(c["id"] for c in snapshot()["charBag"]["chars"]
               if c.get("lv", 1) == 1 and not c.get("lvBreak") and not c.get("rank") and c["id"] // 100 == 103)
    give(30001, 100)
    give(1, 100000)
    st, _ = call("POST", "/g/char/skill/lvup", field_int(1, cid) + field_int(2, 2), token)
    check("an SR's first passive is open at 0 stars but cannot level up there (UnlockPassiveSkillLevelUpCount 0)",
          char(cid)["skillLv"][2] == 1 and st == 400, f"{char(cid)['skillLv']}, status {st}")
    give(5, 100000)
    give(1, 1000000)
    st, body = call("POST", "/g/char/enhance", field_int(1, cid) + field_int(2, 4), token)
    r = decode("character.EnhanceResponse", body) if st == 200 else {}
    want = sum(lvup[l]["Exp"] for l in range(1, 5))
    check("level up 1 -> 5 costs the table's EXP and gold", r.get("lv") == 5 and qty(r.get("lastItems", []), 5) == 100000 - want,
          f"status {st}, {r.get('lv')}, EXP left {qty(r.get('lastItems', []), 5)}")
    st, _ = call("POST", "/g/char/enhance", field_int(1, cid) + field_int(2, 1), token)
    check("level up past the limit break cap is refused", st == 400, f"status {st}")
    for it in (31001, 31101, 31201, 31301):
        give(it, 50)
    st, body = call("POST", "/g/char/lvbreak", field_int(1, cid), token)
    r = decode("character.LvBreakResponse", body) if st == 200 else {}
    check("limit break 1 takes 2500 gold and 3 + 3 materials",
          r.get("lvBreak") == 1 and qty(r.get("lastItems", []), 31001) == 47 and qty(r.get("lastItems", []), 1) == 1000000 - sum(lvup[l]["Money"] for l in range(1, 5)) - 2500,
          f"status {st}, {r}")
    st, _ = call("POST", "/g/char/lvbreak", field_int(1, cid), token)
    check("limit break 2 waits for the level cap", st == 400, f"status {st}")
    st, body = call("POST", "/g/char/enhance", field_int(1, cid) + field_int(2, 5), token)
    check("after limit break the character levels to 10", st == 200 and decode("character.EnhanceResponse", body).get("lv") == 10)

    give(cid, 7)
    st, body = call("POST", "/g/char/rankup", field_int(1, cid) + field_int(3, 1), token)
    r = decode("character.RankUpResponse", body) if st == 200 else {}
    check("evolve to rank 1 takes 5 shards", r.get("rank") == 1 and qty(r.get("lastItems", []), cid) == 2 and r.get("resultCode") == 1,
          f"status {st}, {r}")
    st, _ = call("POST", "/g/char/rankup", field_int(1, cid) + field_int(3, 2), token)
    check("evolve without enough shards is refused", st == 400, f"status {st}")
    give(30003, 10)
    st, body = call("POST", "/g/char/rankup", field_int(1, cid) + field_int(3, 2) + field_int(4, 1), token)
    r = decode("character.RankUpResponse", body) if st == 200 else {}
    check("universal shards cover the rest", r.get("rank") == 2 and qty(r.get("lastItems", []), 30003) == 7, f"status {st}, {r}")

    before = char(cid)["skillLv"]
    raw = open(os.path.join(RAW_TABLES, "SkillLv_C.bytes"), "rb").read()
    rows = [struct.unpack_from("<13i", raw, 4 + 52 * i) for i in range(struct.unpack_from("<i", raw)[0])]
    cost = next(r for r in rows if r[0] == cid and r[2] == 1 and r[1] == before[0])
    for it, n in zip(cost[3::2], cost[4::2]):
        if it:
            give(it, n)
    st, body = call("POST", "/g/char/skill/lvup", field_int(1, cid) + field_int(2, 0), token)
    r = decode("character.EnhanceResponse", body) if st == 200 else {}
    check("skill 1 levels up", st == 200 and r.get("lv") == before[0] + 1, f"status {st}, {r}, before {before}")
    st, _ = call("POST", "/g/char/skill/lvup", field_int(1, cid) + field_int(2, 6), token)
    check("a closed skill cannot level up", st == 400, f"status {st}")

    gems = qty(snapshot()["itemBag"]["items"], 3)
    dex = next(c["id"] for c in snapshot()["charBag"]["chars"] if c.get("flag", 0) & 64)
    st, body = call("POST", "/g/char/collect/reward", field_int(1, dex) + field_int(2, 6), token)
    check("Hunk Dex story 1 reward gives 5 gems", st == 200 and qty(decode("character.CollectRewardResponse", body)["lastItems"], 3) == gems + 5,
          f"status {st}")
    st, _ = call("POST", "/g/char/collect/reward", field_int(1, dex) + field_int(2, 6), token)
    check("the Dex reward is given once (the claim clears its flag)", st == 400 and not char(dex).get("flag", 0) & 64, f"status {st}")

    c = char(cid)
    check("progress is saved on the account", c.get("lv") == 10 and c.get("lvBreak") == 1 and c.get("rank") == 2
          and c["skillLv"][0] == before[0] + 1, f"{c}")

    # sigils: unlock at rank 30 for 1 Sigil Key, improve rolls from RuneRank_C, apply keeps the roll
    give(30011, 2)
    st, _ = call("POST", "/g/char/rune/unlock", field_int(1, cid), token)
    check("a sigil cannot be unlocked before rank 30", st == 400, f"status {st}")
    give(cid, 999)
    give(1, 1000000)
    st, body = call("POST", "/g/char/rankup", field_int(1, cid) + field_int(3, 30), token)
    check("evolve straight to rank 30", st == 200 and decode("character.RankUpResponse", body).get("rank") == 30, f"status {st}")
    check("stars 1-3 crossed open story rewards 2-4 (flags 7-9)", char(cid).get("flag", 0) & 0b1110000000 == 0b1110000000,
          f"flag {char(cid).get('flag')}")
    gems = qty(snapshot()["itemBag"]["items"], 3)
    st, body = call("POST", "/g/char/collect/reward", field_int(1, cid) + field_int(2, 7), token)
    check("story 2's reward gives 5 gems and clears flag 7", st == 200
          and qty(decode("character.CollectRewardResponse", body)["lastItems"], 3) == gems + 5 and not char(cid).get("flag", 0) & 128,
          f"status {st}")
    st, _ = call("POST", "/g/char/collect/reward", field_int(1, cid) + field_int(2, 10), token)
    check("an H scene reward not reached cannot be claimed", st == 400, f"status {st}")
    # outfit items: the Unlock button uses the item (g/item/consume) and the outfit joins the buddy's outfits
    if 2 not in char(10414).get("skins", []):
        give(37414, 1)
        use = field_int(1, 37414) + varint(2 << 3 | 2) + varint(len(field_int(1, 1))) + field_int(1, 1)
        st, body = call("POST", "/g/item/consume", varint(1 << 3 | 2) + varint(len(use)) + use, token)
        r = decode("item.ConsumeItemResponse", body) if st == 200 else {}
        check("using an outfit item unlocks the outfit and uses the item", st == 200 and 2 in char(10414).get("skins", [])
              and qty(r.get("items", []), 37414) == 0, f"status {st}, {r}, {char(10414).get('skins')}")
        give(37414, 1)
        st, _ = call("POST", "/g/item/consume", varint(1 << 3 | 2) + varint(len(use)) + use, token)
        check("an outfit already unlocked refuses its item", st == 400, f"status {st}")
    st, body = call("POST", "/g/char/rune/unlock", field_int(1, cid), token)
    r = decode("character.RuneUnlockResponse", body) if st == 200 else {}
    check("unlocking the sigil takes 1 Sigil Key", st == 200 and qty(r.get("lastItems", []), 30011) == 1, f"status {st}, {r}")
    st, _ = call("POST", "/g/char/rune/unlock", field_int(1, cid), token)
    check("a sigil unlocks once", st == 400, f"status {st}")
    def raw_rows(name, width):
        b = open(os.path.join(RAW_TABLES, name + ".bytes"), "rb").read()
        return [struct.unpack_from(f"<{width}i", b, 4 + 4 * width * i) for i in range(struct.unpack_from("<i", b)[0])]

    b = open(os.path.join(RAW_TABLES, "Character_C.bytes"), "rb").read()
    at, info = 4, {}
    for _ in range(struct.unpack_from("<i", b)[0]):
        v = struct.unpack_from("<11i", b, at)
        at += 45 + b[at + 44] + 4
        info[v[0]] = (v[3], v[4])
    rarity, element = info[cid]
    row = next(r for r in raw_rows("RuneRank_C", 12) if r[:4] == (rarity, element, 0, 1))
    prices = {r[0]: (r[1], r[2:]) for r in raw_rows("RunePriceDiscrim_C", 12)}
    ink = next(prices[i] for i in row[9:11] if prices[i][0] == 30012)[1]
    give(30012, 10000)
    give(5, 1000000)
    st, body = call("POST", "/g/char/rune/improve", field_int(1, cid), token)
    r = decode("character.RuneImproveResponse", body) if st == 200 else {}
    t = r.get("temp", {})
    check("a normal improve rolls inside the RuneRank_C ranges and costs the first price",
          st == 200 and r.get("normalCount") == 1 and row[7] <= t.get("hp", 0) <= row[8] and row[5] <= t.get("atk", 0) <= row[6]
          and qty(r.get("lastItems", []), 30012) == 10000 - ink[0], f"status {st}, {r}, row {row}")
    st, body = call("POST", "/g/char/rune/improve", field_int(1, cid), token)
    r2 = decode("character.RuneImproveResponse", body) if st == 200 else {}
    t2 = r2.get("temp", {})
    check("a second improve before applying rolls again on the applied bonus (the client only offers apply or cancel)"
          " and costs the next price", row[7] <= t2.get("hp", 0) <= row[8] and row[5] <= t2.get("atk", 0) <= row[6]
          and qty(r2.get("lastItems", []), 30012) == 10000 - ink[0] - ink[1], f"{r2}")
    st, body = call("POST", "/g/char/rune/apply", field_int(1, cid) + field_int(2, 1), token)
    kept = decode("character.RuneImproveResponse", body) if st == 200 else {}
    rune = char(cid).get("rune", {})
    check("apply keeps the roll, answers the day's counts and it is saved",
          st == 200 and kept.get("temp") == r2.get("temp") and kept.get("normalCount") == 2 and rune.get("bonus") == r2.get("temp")
          and not rune.get("temp") and rune.get("improveCount") == 2, f"status {st}, {kept}, {rune}")
    give(2, 1000)
    used = 0
    for _ in range(11):
        st, body = call("POST", "/g/char/rune/improve", field_int(1, cid) + field_int(2, 1), token)
        used += st == 200
    check("10 refines a day, normal and special together", used == 8, f"{used} special after 2 normal")

    # profile: rename once a week for a Player Name Ticket, other edits saved as they are
    give(21011, 2)
    st, _ = call("POST", "/g/userinfo", field_str(1, "WoofTester"), token)
    check("a rename within 7 days of the last one is refused", st == 400, f"status {st}")
    call("POST", "/admin/rename-reset", f"account={acc_id}".encode(), host="127.0.0.1")
    st, _ = call("POST", "/g/userinfo", field_str(1, "Bad Name!"), token)
    check("a name with symbols is refused", st == 400, f"status {st}")
    st, _ = call("POST", "/g/userinfo", field_str(1, "Andrés"), token)
    st2, _ = call("POST", "/g/userinfo", field_str(1, "SmokeName"), token)
    st3, _ = call("POST", "/g/userinfo", field_str(1, "狼狼狼狼狼狼狼狼"), token)
    check("names follow the client: no accented letters, no banned word (\"sm\"), 14 wide (CJK counts 2)",
          st == 400 and st2 == 400 and st3 == 400, f"{st}, {st2}, {st3}")
    st, body = call("POST", "/g/userinfo", field_str(1, "WoofTester"), token)
    r = decode("userinfo.SetUserInfoResponse", body) if st == 200 else {}
    check("after the admin reset a rename takes 1 ticket", r.get("code") == 1 and qty([r.get("remainingItem", {})], 21011) == 1,
          f"status {st}, {r}")
    st, _ = call("POST", "/g/userinfo", field_str(1, "SmokeAgain"), token)
    check("the next rename waits again", st == 400, f"status {st}")
    give(64141, 1)
    give(56002, 1)
    title0 = snapshot()["basic"].get("title", 0)
    call("POST", "/g/userinfo", field_str(2, "smoke message"), token)
    for icon in (field_int(1, 64141), field_int(2, 56002)):
        call("POST", "/g/userinfo", varint(3 << 3 | 2) + varint(len(icon)) + icon, token)
    b = snapshot()["basic"]
    check("name, message and icon are saved; an icon change keeps the parts it does not carry", b.get("name") == "WoofTester"
          and b.get("intro") == "smoke message" and b.get("pic") == 64141 and b.get("frame") == 56002 and b.get("title", 0) == title0,
          f"{ {k: b.get(k) for k in ('name', 'intro', 'pic', 'frame', 'title')} }")
    give(56003, 0)
    icon = field_int(2, 56003)
    st, _ = call("POST", "/g/userinfo", varint(3 << 3 | 2) + varint(len(icon)) + icon, token)
    check("a frame not owned is refused", st == 400, f"status {st}")

    # Sausage Bro (as the client's SausageManager has it): three records, one per 12-hour window from the 21:00 UTC
    # reset, and a bonus record only with the Economy Pass; direct (50) or by dice (sum x 5, doubled for a triple).
    # The test stays within today (a later day would make other day-bound records for that day).
    call("POST", "/admin/clock", b"reset=1", host="127.0.0.1")
    into_day = (int(time.time()) - 75600) % 86400
    w = 0 if into_day < 43200 else 1
    energy = qty(snapshot()["itemBag"]["items"], 4)
    st, body = call("GET", "/g/mall/sausage/0", token=token)
    r = decode("sausage.GetSausageResponse", body) if st == 200 else {}
    check(f"Sausage Bro direct claim gives 50 energy in this window's record ({w})", r.get("recIndex", 0) == w
          and qty([r.get("finalItem", {})], 4) == energy + 50 and len(r.get("sausageRec", {}).get("diceRecs", [])) == 3
          and not r.get("sausageRec", {}).get("hasBonus"), f"status {st}, {r}")
    st, _ = call("GET", "/g/mall/sausage/1", token=token)
    check("one claim per window without the Economy Pass", st == 400, f"status {st}")
    if w == 0:
        call("POST", "/admin/clock", f"secs={43200 - into_day + 60}".encode(), host="127.0.0.1")  # window 1, same day
        st, body = call("GET", "/g/mall/sausage/1", token=token)
        r = decode("sausage.GetSausageResponse", body) if st == 200 else {}
        rec = r.get("sausageRec", {}).get("diceRecs", [{}, {}])[1] if r else {}
        dice = rec.get("dice", [])
        want = sum(dice) * 5 * (2 if len(set(dice)) == 1 else 1)
        check("the second window's dice roll (record 1) gives the dice sum x 5", r.get("recIndex") == 1 and len(dice) == 3
              and all(1 <= d <= 6 for d in dice) and rec.get("getEnergy") == want and qty([r.get("rewardItem", {})], 4) == want,
              f"status {st}, {r}")
        call("POST", "/admin/clock", b"reset=1", host="127.0.0.1")
    recs = snapshot()["mall"]["sausageRec"]["diceRecs"]
    check("no bonus record without the pass; claims are saved", len(recs) == 3 and recs[w].get("getEnergy")
          and not recs[2].get("getEnergy"), f"{recs}")

    st, body = call("POST", "/admin/bond", f"account={acc_id}&char=10414&lv=max".encode(), host="127.0.0.1")
    f = char(10414).get("femdom", {})
    check("admin sets an SSR's bond to max (level 8)", f.get("lv") == 8 and not f.get("exp"), f"{f.get('lv')}, {f.get('exp')}")
    call("POST", "/admin/bond", f"account={acc_id}&char=10414&lv=2&exp=1234".encode(), host="127.0.0.1")
    f = char(10414).get("femdom", {})
    check("admin sets a bond level with EXP into it", f.get("lv") == 2 and f.get("exp") == 1234, f"{f.get('lv')}, {f.get('exp')}")
    call("POST", "/admin/bond", f"account={acc_id}&char=10414&lv=max".encode(), host="127.0.0.1")

    # stages: energy cost, stars from the conditions, drops from Stage_Reward_C, account EXP, sweeps and losses
    def read_stages():
        b = open(os.path.join(RAW_TABLES, "Stage_C.bytes"), "rb").read()
        at = [4]

        def i32():
            v = struct.unpack_from("<i", b, at[0])[0]
            at[0] += 4
            return v

        def skip_str():
            at[0] += 1 + b[at[0]]
        out = {}
        for _ in range(struct.unpack_from("<i", b)[0]):
            ch, sid = i32(), i32()
            skip_str()
            prev = i32()
            i32()
            for _ in range(3):
                skip_str()
            i32(), i32()
            cost = i32()
            at[0] += 8 + 8
            nb = b[at[0]]
            at[0] += 1 + 4 * nb
            at[0] += 4 * 6
            out[sid] = dict(ch=ch, prev=prev, cost=cost, battle=nb > 0)
        return out
    stages = read_stages()

    def stage_recs():
        return {x["stageId"]: x for x in snapshot().get("stage", {}).get("stage", [])}

    def run(ch, sid, res=b"", tickets=0):
        st, body = call("POST", "/g/stage/", field_int(1, ch) + field_int(2, sid) + field_int(3, 1) + field_int(4, tickets), token)
        if st != 200:
            return st, {}
        sn = decode("stage.DoStageResponse", body)["stageSn"]
        st, body = call("POST", "/g/stage/over", field_str(1, sn) + varint(3 << 3 | 2) + varint(len(res)) + res, token)
        return st, decode("stage.StageOverResponse", body) if st == 200 else {}
    give(4, 100)
    give(21001, 10)
    recs = stage_recs()
    three = next(sid for sid, x in sorted(recs.items()) if x.get("flag") == 15 and stages.get(sid, {}).get("cost") == 10)
    ch = stages[three]["ch"]
    b0 = snapshot()["basic"]
    good = field_int(5, 3)
    st, r = run(ch, three, good)
    lists = r.get("rewardList", [])
    energy = qty(r.get("finalItems", []), 4)
    check("a cleared stage again: 10 energy, one drop list with 10 account EXP", st == 200 and r.get("isWin") and not r.get("firstClear")
          and len(lists) == 1 and qty(lists[0].get("items", []), 6) == 10 and energy == 90, f"status {st}, {r}")
    b1 = snapshot()["basic"]
    check("account EXP goes up by the energy spent", (b1.get("lv"), b1.get("exp", 0)) != (b0.get("lv"), b0.get("exp", 0)), f"{b0.get('exp')} -> {b1.get('exp')}")
    # the client reads account EXP from item 6 in the bag (PlayerData.get_Exp), so the bag keeps Basic.exp
    snap = snapshot()
    check("item 6 in the bag matches the account EXP", qty(snap["itemBag"]["items"], 6) == snap["basic"].get("exp", 0),
          f"item 6 {qty(snap['itemBag']['items'], 6)}, Basic.exp {snap['basic'].get('exp', 0)}")
    lvs = {r["Lv"]: r for r in json.load(open(os.path.join(TABLES, "LvUp_C.json"), encoding="utf-8"))["rows"]}
    lv0, e0 = snap["basic"].get("lv", 1), qty(snap["itemBag"]["items"], 4)
    give(6, lvs[lv0]["needExp"] + 5)
    snap = snapshot()
    check("enough account EXP levels up once, keeps the rest and gives the level's energy",
          snap["basic"].get("lv") == lv0 + 1 and snap["basic"].get("exp") == 5 and qty(snap["itemBag"]["items"], 6) == 5
          and qty(snap["itemBag"]["items"], 4) == e0 + lvs[lv0]["EnergyReward"],
          f"lv {lv0} -> {snap['basic'].get('lv')}, exp {snap['basic'].get('exp')}, energy {e0} -> {qty(snap['itemBag']['items'], 4)}")
    give(4, 90)
    st, r = run(ch, three, field_int(11, 1))
    check("a retreat is a loss and gives the energy back", st == 200 and not r.get("isWin") and qty(r.get("finalItems", []), 4) == 90, f"{r}")
    st, r = run(ch, three, b"", tickets=3)
    check("Fast Pass x3 runs the stage 3 times", st == 200 and len(r.get("rewardList", [])) == 3
          and qty(r.get("finalItems", []), 21001) == 7 and qty(r.get("finalItems", []), 4) == 60, f"status {st}, {r}")
    st, _ = call("POST", "/g/stage/over", field_str(1, "nope!"), token)
    check("finishing a run that was not started is refused", st == 400, f"status {st}")
    st, _ = call("POST", "/g/stage/", field_int(1, ch + 1) + field_int(2, three) + field_int(3, 1), token)
    check("a stage asked for under the wrong chapter is refused", st == 400, f"status {st}")
    nxt = next((sid for sid, x in sorted(stages.items()) if x["ch"] in (1, 2, 3, 4) and sid not in recs
                and (x["prev"] == 0 or (x["prev"] in recs and recs[x["prev"]].get("flag", 0) & 1))), None)
    check("there is a next stage to clear", nxt is not None)
    if nxt:
        locked = next((sid for sid, x in sorted(stages.items()) if x["prev"] == nxt), None)
        if locked:
            st, _ = call("POST", "/g/stage/", field_int(1, stages[locked]["ch"]) + field_int(2, locked) + field_int(3, 1), token)
            check("a stage after an uncleared one is refused", st == 400, f"stage {locked}, status {st}")
        st, r = run(stages[nxt]["ch"], nxt, good)
        want = 15 if stages[nxt]["battle"] else 1
        check("a new stage record has its (empty) multi-battle status, as the client needs",
              "multiBattleStatus" in stage_recs().get(nxt, {}), f"{stage_recs().get(nxt)}")
        check("a first clear gives the first-clear reward and the stage keeps its stars",
              st == 200 and r.get("firstClear") and r.get("flag") == want and stage_recs().get(nxt, {}).get("flag") == want, f"stage {nxt}, {r}")
    # event stages: Stage_C chapters 10011 and up, through /g/stage/event/ (captured: chapter 10044, stage 1004401,
    # a story stage, first clear 5 gems, saved with flag 1); new accounts already have 10044's first two
    recs = stage_recs()
    ev_first = next(sid for sid, x in sorted(stages.items()) if x["ch"] >= 10011 and x["prev"] == 0
                    and not any(stages.get(r, {}).get("ch") == x["ch"] for r in recs))
    ev_ch = stages[ev_first]["ch"]
    st, body = call("POST", "/g/stage/event/", field_int(1, ev_ch) + field_int(2, ev_first) + field_int(3, 1), token)
    sn = decode("stage.DoStageResponse", body).get("stageSn", "") if st == 200 else ""
    res = field_int(5, 8) + field_int(6, 4937)
    st, body = call("POST", "/g/stage/event/over", field_str(1, sn) + varint(3 << 3 | 2) + varint(len(res)) + res, token)
    r = decode("stage.StageOverResponse", body) if st == 200 else {}
    rec = stage_recs().get(ev_first, {})
    check(f"an event stage ({ev_first}) is cleared and saved under its chapter",
          st == 200 and r.get("firstClear") and r.get("isWin") and rec.get("chapterId") == ev_ch and rec.get("flag", 0) & 1,
          f"status {st}, {r}, {rec}")
    second = next((sid for sid, x in stages.items() if x["prev"] == ev_first), None)
    third = next((sid for sid, x in stages.items() if second and x["prev"] == second), None)
    if third:
        st, _ = call("POST", "/g/stage/event/", field_int(1, ev_ch) + field_int(2, third) + field_int(3, 1), token)
        check("an event stage after an uncleared one is refused", st == 400, f"stage {third}, status {st}")
    # tutorial steps (PATCH /g/guide/{n} sets bit n - 1, as captured) and the energy check
    st, _ = call("PATCH", "/g/guide/12", b"", token)
    check("a tutorial step sets its bit", st == 200 and int(snapshot()["basic"].get("guideRec", 0)) >> 11 & 1 == 1, f"status {st}")
    st, body = call("PATCH", "/g/guide/skip", b"", new_token)
    t = decode("character.TeamInfo", body) if st == 200 else {}
    rec = int(gsave_basic(new_token).get("guideRec", 0))
    check("skipping the tutorial (PATCH, as the game sends it) marks steps 1-9 and answers a team (the later ones still run)",
          st == 200 and t.get("idx", 0) >= 1 and rec & 0x1FF == 0x1FF and not rec >> 9 & 1, f"status {st}, {t}, {rec:#x}")
    st, body = call("GET", "/g/energy", token=token)
    e = decode("synchronous.EnergyCheck", body) if st == 200 else {}
    check("the energy check answers the energy held", st == 200 and e.get("nowEnergy", 0) == qty(snapshot()["itemBag"]["items"], 4),
          f"status {st}, {e}")

    # buying with gems at the PriceDiscrim_C prices (energy 5 then 10; Hard tries 5; challenge 15; hookups 15)
    give(3, 1000)
    give(4, 0)
    st, body = call("POST", "/g/stage/buy/energy", field_int(1, 3) + field_int(3, 1), token)
    r = decode("stage.BuyEnergyResponse", body) if st == 200 else {}
    st2, body2 = call("POST", "/g/stage/buy/energy", field_int(1, 3) + field_int(3, 1), token)
    r2 = decode("stage.BuyEnergyResponse", body2) if st2 == 200 else {}
    check("energy for gems: 60 a buy, 5 then 10 gems", st == 200 and r.get("purchaseTimes") == 1 and qty(r.get("lastItem", []), 3) == 995
          and qty(r.get("lastItem", []), 4) == 60 and qty(r2.get("lastItem", []), 3) == 985, f"{r}, {r2}")
    hard = next(sid for sid, x in sorted(stages.items()) if 101 <= x["ch"] <= 199)
    st, body = call("POST", "/g/stage/buy/hard", field_int(1, stages[hard]["ch"]) + field_int(2, hard) + field_int(4, 1), token)
    r = decode("stage.BuyHardTimesResponse", body) if st == 200 else {}
    check("Hard stage tries for gems: 5 gems the first buy", st == 200 and r.get("purchaseTimes") == 1 and qty(r.get("lastItem", []), 3) == 980,
          f"status {st}, {r}")
    st, body = call("POST", "/g/stage/buy/challenge", field_int(1, 1) + field_int(3, 1), token)
    r = decode("stage.BuyChallengeTimesResponse", body) if st == 200 else {}
    check("challenge tries for gems: 15 gems the first buy", st == 200 and r.get("purchaseTimes") == 1 and qty(r.get("lastItem", []), 3) == 965,
          f"status {st}, {r}")
    st, body = call("POST", "/g/yiff/femdom/purchase", field_int(1, 1), token)
    r = decode("yiff.FemdomPurchaseResponse", body) if st == 200 else {}
    check("a hookup entry for gems: 15 gems the first buy of the day", st == 200 and r.get("purchaseTimes") == 1
          and qty(r.get("lastItem", []), 3) == 950, f"status {st}, {r}")
    for _ in range(2):
        call("POST", "/g/yiff/femdom/purchase", field_int(1, 1), token)
    st, _ = call("POST", "/g/yiff/femdom/purchase", field_int(1, 1), token)
    check("hookup entries for gems stop at 3 a day", st == 400 and qty(snapshot()["itemBag"]["items"], 3) == 905, f"status {st}")

    ch_recs = {x["chapterId"]: x.get("flag", 0) for x in snapshot().get("stage", {}).get("chapter", [])}
    st, body = call("POST", "/g/stage/ch_prize", field_int(1, 1) + field_int(2, 1), token)
    st2, _ = call("POST", "/g/stage/ch_prize", field_int(1, 1) + field_int(2, 1), token)
    check("a chapter prize is claimed once", (st == 200 or ch_recs.get(1, 0) & 2) and st2 == 400, f"status {st}, then {st2}")

    st, body = call("POST", "/admin/maxout", f"account={acc_id}&rarity=4".encode(), host="127.0.0.1")
    c = char(10414)
    check("admin maxes out every XXL: level 60, limit break 11, rank 75, skills 10, bond 8, sigil at its limit",
          c.get("lv") == 60 and c.get("lvBreak") == 11 and c.get("rank") == 75 and c["skillLv"][:2] == [10, 10]
          and all(x > 1 for x in c["skillLv"]) and c.get("femdom", {}).get("lv") == 8 and c.get("rune", {}).get("unlock")
          and sum(c["rune"]["bonus"].values()) == 20000, f"{ {k: c.get(k) for k in ('lv', 'lvBreak', 'rank', 'skillLv', 'rune')} }")
    check("a maxed XXL has its bond and 5-star outfits", char(10414).get("skins") == [0, 1, 2], f"{char(10414).get('skins')}")
    st, _ = call("POST", "/g/char/skin", field_int(1, 10414) + field_int(2, 2), token)
    check("wearing an unlocked outfit is saved", st == 200 and char(10414).get("skin") == 2, f"status {st}")
    low = next(c["id"] for c in snapshot()["charBag"]["chars"] if c.get("rank", 0) == 0 and not c.get("femdom", {}).get("lv"))
    st, _ = call("POST", "/g/char/skin", field_int(1, low) + field_int(2, 2), token)
    check("a locked outfit cannot be worn", st == 400, f"character {low}, status {st}")
    team = field_int(1, 2) + b"".join(field_int(2, x) for x in (10414, 10001))
    st, body = call("POST", "/g/team/battle", team, token)
    saved = next((x for x in snapshot()["charBag"].get("battleTeams", []) if x.get("idx") == 2), {})
    check("a battle team is saved", st == 200 and saved.get("chars") == [10414, 10001], f"status {st}, {saved}")
    # resets (no captures): level back to 1 and no limit break, a skill back to level 1, the costs returned
    rc = next(c for c in snapshot()["charBag"]["chars"] if c.get("lv") == 60 and c["id"] != 10414)
    give(21015, 0)
    st, _ = call("POST", "/g/char/reset/skill", field_int(1, rc["id"]) + field_int(2, 0), token)
    check("a skill reset needs the reset item (21015)", st == 400 and char(rc["id"])["skillLv"][0] == 10, f"status {st}")
    give(21015, 1)
    st, body = call("POST", "/g/char/reset/skill", field_int(1, rc["id"]) + field_int(2, 0), token)
    r = decode("character.ResetSkillResponse", body) if st == 200 else {}
    check("a skill reset puts the skill back to level 1, returns its costs and takes the item",
          st == 200 and char(rc["id"])["skillLv"][0] == 1 and r.get("returnItems") and qty(r.get("lastItems", []), 21015) == 0,
          f"status {st}, {r}")
    give(21014, 0)
    st, _ = call("POST", "/g/char/reset/lv", field_int(1, rc["id"]), token)
    check("a level reset needs the reset item (21014)", st == 400 and char(rc["id"]).get("lv") == 60, f"status {st}")
    give(21014, 1)
    gold0 = qty(snapshot()["itemBag"]["items"], 1)
    st, body = call("POST", "/g/char/reset/lv", field_int(1, rc["id"]), token)
    r = decode("character.ResetLvResponse", body) if st == 200 else {}
    after = char(rc["id"])
    back_gold = qty(r.get("returnItems", []), 1)
    check("a level reset: level 1, no limit break, EXP, gold and limit break materials back",
          st == 200 and after.get("lv") == 1 and not after.get("lvBreak") and back_gold > 0
          and qty(r.get("lastItems", []), 1) == gold0 + back_gold and qty(r.get("returnItems", []), 5) > 0
          and len(r.get("returnItems", [])) > 2 and qty(r.get("lastItems", []), 21014) == 0, f"status {st}, {r}, lv {after.get('lv')}")
    st, _ = call("POST", "/g/char/reset/lv", field_int(1, rc["id"]), token)
    check("a character at level 1 cannot be reset again", st == 400, f"status {st}")

    # part-time jobs (dispatch): a new board each game day, start, cancel, claim, refresh
    board = snapshot().get("dispatch", {})
    jobs = board.get("dispatch", [])
    import datetime as _dt
    today = _dt.datetime.fromtimestamp(((int(time.time()) - 75600) // 86400) * 86400, _dt.timezone.utc).strftime("%Y%m%d")
    check("a part-time job board of the day: 5 jobs with the date in their ids", len(jobs) == 5
          and all(str(j["id"]).startswith(today) for j in jobs), f"{jobs}")
    free = [c["id"] for c in snapshot()["charBag"]["chars"]][:3]
    st, body = call("POST", "/g/dispatch/start", varint(1 << 3 | 2) + varint(len(field_int(1, jobs[0]["id"]) + field_int(2, free[0])))
                    + field_int(1, jobs[0]["id"]) + field_int(2, free[0]), token)
    r = decode("dispatch.DispatchStartResponse", body) if st == 200 else {}
    check("a job starts and ends after its duration", st == 200 and int(r.get("dispatches", [{}])[0].get("endTimeUnix", 0)) > time.time() + 1000
          and r.get("refreshableNum") == 4, f"status {st}, {r}")
    st, _ = call("POST", "/g/dispatch/start", varint(1 << 3 | 2) + varint(len(field_int(1, jobs[1]["id"]) + field_int(2, free[0])))
                 + field_int(1, jobs[1]["id"]) + field_int(2, free[0]), token)
    check("a buddy on a job cannot take a second one", st == 400, f"status {st}")
    st, body = call("POST", "/g/dispatch/cancel", field_int(1, jobs[0]["id"]), token)
    r = decode("dispatch.DispatchCancelResponse", body) if st == 200 else {}
    check("cancelling calls the buddy back", st == 200 and r.get("teamCharacterIds") == [free[0]], f"status {st}, {r}")
    for k in range(2):
        nd = field_int(1, jobs[k]["id"]) + field_int(2, free[k])
        call("POST", "/g/dispatch/start", varint(1 << 3 | 2) + varint(len(nd)) + nd, token)
    st, _ = call("POST", "/g/dispatch/claim", b"", token)
    check("an unfinished job cannot be claimed", st == 400, f"status {st}")
    call("POST", "/admin/dispatch-finish", f"account={acc_id}".encode(), host="127.0.0.1")
    st, body = call("POST", "/g/dispatch/claim", b"", token)
    r = decode("dispatch.DispatchClaimResponse", body) if st == 200 else {}
    left = snapshot().get("dispatch", {}).get("dispatch", [])
    check("claiming pays the finished jobs (item 10 at least) and takes them off the board",
          st == 200 and len(r.get("ids", [])) == 2 and qty(r.get("rewards", []), 10) > 0 and len(left) == 3, f"status {st}, {r}")
    before = {j["id"]: j["taskId"] for j in left}
    give(3, 100)
    st, body = call("POST", "/g/dispatch/refresh", b"", token)
    r = decode("dispatch.DispatchRefreshResponse", body) if st == 200 else {}
    st2, body2 = call("POST", "/g/dispatch/refresh", b"", token)
    r2 = decode("dispatch.DispatchRefreshResponse", body2) if st2 == 200 else {}
    check("a refresh costs Prices[refreshes so far] (5, then 5 gems) and keeps the jobs' ids (the client overwrites by id)",
          st == 200 and r.get("refreshCount") == 1 and qty(r.get("remainingItem", []), 3) == 95
          and {j["id"] for j in r.get("dispatches", [])} == set(before) and st2 == 200
          and qty(r2.get("remainingItem", []), 3) == 90, f"{st}, {r}; {st2}, {r2}")

    # Bingo (lottery 7501): cells show their reward (cell n = reward n + 1), 1 ticket (20006) each; lines pay rewards 17-26
    st, body = call("GET", "/g/event/lottery/?id=7501", token=token)
    rec = (decode("activity.GetLotteryRecResponse", body).get("lotteryRecs") or [{}])[0] if st == 200 else {}
    check("the Bingo record starts empty and skippable", st == 200 and rec.get("lotteryId") == 7501 and not rec.get("records")
          and rec.get("skippable"), f"status {st}, {rec}")
    give(20006, 0)
    poke = lambda pos: call("POST", "/g/event/lottery/poke", field_int(1, 7501) + field_int(2, 1) + field_int(3, pos), token)
    st, _ = poke(0)
    check("opening a Bingo cell needs a ticket", st == 400, f"status {st}")
    give(20006, 20)
    gems0 = qty(snapshot()["itemBag"]["items"], 3)
    got = []
    for pos in range(4):
        st, body = poke(pos)
        got.append(decode("activity.PokeLotteryResponse", body) if st == 200 else {})
    check("cell 2 (reward 3) gives its 10 gems and takes a ticket", qty(got[2].get("gainItems", []), 3) == 10
          and got[2].get("records", [{}])[0].get("rewardId") == 3 and qty(got[3].get("lastItems", []), 20006) == 16, f"{got[2]}, {got[3]}")
    st, _ = poke(1)
    check("a cell opens once", st == 400, f"status {st}")
    st, body = call("GET", "/g/event/lottery?id=7501", token=token)
    rec = (decode("activity.GetLotteryRecResponse", body).get("lotteryRecs") or [{}])[0] if st == 200 else {}
    check("the first row is a connected line (line bit 1)", len(rec.get("records", [])) == 4 and rec.get("lineConnected", 0) == 2, f"{rec}")
    st0, _ = call("GET", "/g/event/lottery/claim_line/7501/16", token=token)
    st, body = call("GET", "/g/event/lottery/claim_line/7501/17", token=token)
    r = decode("activity.ClaimBingoLineRewardResponse", body) if st == 200 else {}
    st2, _ = call("GET", "/g/event/lottery/claim_line/7501/17", token=token)
    check("row 0's reward (index 17 = reward 18, a Golden Bell) is claimed once; an open line cannot be",
          st0 == 400 and st == 200 and qty(r.get("gainItems", []), 20002) == 1 and st2 == 400, f"{st0}, status {st}, {r}, then {st2}")
    st, body = call("POST", "/g/event/lottery/autopoke", field_int(1, 7501), token)
    r = decode("activity.AutoPokeLotteryResponse", body) if st == 200 else {}
    check("auto open fills the rest of the card with the tickets left", st == 200 and len(r.get("records", [])) == 12
          and qty(r.get("lastItems", []), 20006) == 4, f"status {st}, {len(r.get('records', []))}")

    # BBQ (event 10085): every mix of up to 5 of items 141-143 is a recipe; claim gives its combine_reward_C drop
    st, body = call("GET", "/g/event/combine?event_id=10085", token=token)
    recs = decode("activity.GetCombineRecResponse", body).get("combineRecs", []) if st == 200 else []
    check("the BBQ grill starts empty", st == 200 and recs and recs[0].get("eventId") == 10085 and not recs[0].get("items"), f"{recs}")
    for i in (141, 142, 143):
        give(i, 5)
    grill = lambda items: call("POST", "/g/event/combine/start", field_int(1, 10085) + b"".join(field_int(2, i) for i in items), token)
    st, body = grill([141, 141, 142])
    r = decode("activity.CombineStartResponse", body) if st == 200 else {}
    end = int(r.get("combineRec", {}).get("endUnix", 0))
    check("grilling 2 x 141 + 142 takes them and is done in 15 seconds (WaitTime is seconds in the client)", st == 200
          and abs(end - time.time() - 15) < 10 and qty(snapshot()["itemBag"]["items"], 141) == 3, f"status {st}, {r}")
    st, _ = grill([143])
    st2, _ = call("POST", "/g/event/combine/claim", field_int(1, 10085), token)
    check("one thing on the grill at a time; it cannot be claimed early", st == 400 and st2 == 400, f"{st}, {st2}")
    st, body = call("POST", "/g/event/combine/cancel", field_int(1, 10085), token)
    check("cancelling gives the ingredients back", st == 200 and qty(snapshot()["itemBag"]["items"], 141) == 5, f"status {st}")
    st, body = call("POST", "/g/event/combine/cancel", field_int(1, 10085), token)
    r = decode("activity.CombineCancelResponse", body) if st == 200 else {}
    check("cancelling an empty grill answers code 1 (the client's \"cannot cancel\")", st == 200 and r.get("code") == 1, f"status {st}, {r}")
    grill([143])
    call("POST", "/admin/bbq-finish", f"account={acc_id}".encode(), host="127.0.0.1")
    st, body = call("POST", "/g/event/combine/claim", field_int(1, 10085), token)
    r = decode("activity.CombineClaimResponse", body) if st == 200 else {}
    check("a done dish (1 x 143: drop 55) gives 1-3 of item 144 and 1-2 of material 31401", st == 200
          and 1 <= qty(r.get("gainItems", []), 144) <= 3 and 1 <= qty(r.get("gainItems", []), 31401) <= 2, f"status {st}, {r}")
    check("the claim's last items list the cooked ingredient too (the client's only bag update)",
          any(i.get("id") == 143 for i in r.get("lastItems", [])), f"{r.get('lastItems')}")

    # mall records and limits: purchase_C PurchaseLimits per mall page period (captured: free pack 1101 bought once)
    st, body = call("GET", "/g/mall/save", token=token)
    recs = {g["id"]: g for g in decode("mall.MallSaveResponse", body).get("packRecs", [])} if st == 200 else {}
    check("the mall records start from the capture (pack 1101 bought once, until 1792616400)", st == 200
          and recs.get(1101, {}).get("purchaseCnt") == 1 and int(recs[1101].get("expiry", 0)) == 1792616400, f"{recs}")
    st, _ = call("POST", "/g/mall/ingame/purchase", field_int(1, 22041), token)
    check("the free pack cannot be taken twice in its 14 days", st == 400, f"status {st}")
    # the admin page ends its period early: the record goes out as 0 bought (as at the real period end), one more
    # take, then refused again until the next period
    call("POST", "/admin/packs-period-end", f"account={acc_id}&merch=1101".encode(), host="127.0.0.1")
    st, body = call("GET", "/g/mall/save", token=token)
    rec = {g["id"]: g for g in decode("mall.MallSaveResponse", body).get("packRecs", [])}.get(1101, {}) if st == 200 else {}
    st2, _ = call("POST", "/g/mall/ingame/purchase", field_int(1, 22041), token)
    st3, _ = call("POST", "/g/mall/ingame/purchase", field_int(1, 22041), token)
    check("a free pack whose period ended shows 0 bought, is taken once more, then refused", rec.get("purchaseCnt", 0) == 0
          and int(rec.get("expiry", 0)) > time.time() and st2 == 200 and st3 == 400, f"{rec}, {st2}, {st3}")

    # subscriptions (H Coin, 30 days): once reward mailed, daily reward claimed once a game day
    call("POST", "/admin/hcoin", f"account={acc_id}&qty=5000".encode(), host="127.0.0.1")
    st, body = call("POST", "/g/mall/cash/purchase", field_int(2, 31001), token)
    st2, body2 = call("GET", "/g/mall/subscription", token=token)
    subs = {x["id"]: x for x in decode("mall.GetSubscriptionResponse", body2).get("subscriptionRecs", [])} if st2 == 200 else {}
    check("subscription 701 bought: active for 30 days", st == 200 and 701 in subs
          and abs(int(subs[701].get("expiry", 0)) - time.time() - 30 * 86400) < 60 and not subs[701].get("isClaimed"), f"{st}, {subs}")
    st, _ = call("POST", "/g/mall/cash/purchase", field_int(2, 31001), token)
    check("a subscription with more than 10 days left cannot be bought again", st == 400, f"status {st}")
    mask = newmask(call("GET", "/g/checknew", token=token)[1])
    check("an unclaimed monthly card shows the mall's card dot (bit 5)", mask >> 5 & 1, f"{mask:b}")
    st, body = call("POST", "/g/mall/subscription/claim", field_int(1, 701), token)
    r = decode("mall.ClaimSubscriptionResponse", body) if st == 200 else {}
    st2, body2 = call("POST", "/g/mall/subscription/claim", field_int(1, 701), token)
    r2 = decode("mall.ClaimSubscriptionResponse", body2) if st2 == 200 else {}
    check("its daily reward (10 Fast Pass) is claimed once a day (code 1, then 2 = already claimed)", st == 200
          and qty(r.get("rewards", []), 21001) == 10 and r.get("code") == 1 and st2 == 200 and r2.get("code") == 2
          and not r2.get("rewards"), f"status {st}, {r}, then {st2} {r2}")
    mask = newmask(call("GET", "/g/checknew", token=token)[1])
    check("the card dot clears once claimed", not mask >> 5 & 1, f"{mask:b}")
    st, body = call("POST", "/g/mall/subscription/claim", field_int(1, 702), token)
    r = decode("mall.ClaimSubscriptionResponse", body) if st == 200 else {}
    check("an inactive subscription answers code 3 and gives nothing", st == 200 and r.get("code") == 3 and not r.get("rewards"),
          f"status {st}, {r}")

    # growth fund 102 (MerchType 3): buying it opens its level missions (type 21)
    def fund_ids():
        st, body = call("GET", "/g/mission?type=21", token=token)
        return {x["id"] for x in decode("mission.Mission", body).get("inProgress", [])} if st == 200 else set()
    before = fund_ids()
    call("POST", "/admin/hcoin", f"account={acc_id}&qty=10000".encode(), host="127.0.0.1")
    st, _ = call("POST", "/g/mall/cash/purchase", field_int(2, 41001), token)
    after = fund_ids()
    check("buying growth fund 102 opens its level missions", st == 200 and not (before & set(range(15001, 15009)))
          and set(range(15001, 15009)) <= after, f"{st}, {sorted(before)[:5]}, {sorted(after)[:10]}")
    st, body = call("POST", "/g/mission/complete", varint(1 << 3 | 2) + varint(len(varint(15001))) + varint(15001), token)
    r = decode("mission.MissionCompleteResponse", body) if st == 200 else {}
    check("its level 5 mission pays 50 gems", st == 200 and qty(r.get("rewards", []), 3) == 50, f"status {st}, {r.get('rewards')}")

    # tower passes (types 23 free, 24 paid): no mission listed twice, closed seasons (271) left out
    st, body = call("GET", "/g/mission?type=23,24", token=token)
    ids = [x["id"] for x in decode("mission.Mission", body).get("inProgress", [])] if st == 200 else []
    check("the tower pass list has no duplicates and no closed season", st == 200 and len(ids) == len(set(ids))
          and not any(16301 <= i <= 16305 or 16601 <= i <= 16605 for i in ids), f"{sorted(ids)}")

    # battle pass 43001: ranks by pass EXP (item 20); VIP rewards once the pass is bought
    st, _ = call("POST", "/g/mall/cash/purchase", field_int(2, 43001), token)
    check("a pass whose season has ended cannot be bought", st == 400, f"status {st}")
    st, body = call("POST", "/admin/events", b"id=2001", host="127.0.0.1")
    page = body.decode("utf-8", "replace") if isinstance(body, bytes) else str(body)
    check("scheduling pass season 2001 rebuilds the tables", st == 200 and "main_data rebuilt" in page, page[:300])
    give(20, 250)
    give(3, 1000)
    mask = newmask(call("GET", "/g/checknew", token=token)[1])
    check("a pass rank to claim shows the pass dot (bit 6)", mask >> 6 & 1, f"{mask:b}")
    st, body = call("GET", "/g/mall/battlepass", token=token)
    recs = decode("mall.BattlePassResponse", body).get("battlepassRecs", []) if st == 200 else []
    check("the pass list carries a record for the open season", st == 200 and recs, f"status {st}, {recs}")
    st, body = call("POST", "/g/mall/battlepass/claim", field_int(1, 43001), token)
    r = decode("mall.BattlePassClaimResponse", body) if st == 200 else {}
    check("250 pass EXP: ranks 1 and 2 free rewards (3 x 21013, 50000 gold), the flag counts 2 ranks", st == 200 and r.get("baseRewardFlag") == 2
          and not r.get("vipRewardFlag") and qty(r.get("rewards", []), 21013) == 3 and qty(r.get("rewards", []), 1) == 50000, f"{st}, {r}")
    call("POST", "/g/mall/cash/purchase", field_int(2, 43001), token)
    st, body = call("POST", "/g/mall/battlepass/claim", field_int(1, 43001), token)
    r = decode("mall.BattlePassClaimResponse", body) if st == 200 else {}
    check("buying the pass opens the VIP rewards of the ranks reached", st == 200 and r.get("vipRewardFlag") == 2
          and qty(r.get("rewards", []), 21002) == 2, f"{st}, {r}")
    mask = newmask(call("GET", "/g/checknew", token=token)[1])
    check("the pass dot clears once every rank reached is claimed", not mask >> 6 & 1, f"{mask:b}")
    st, _ = call("POST", "/g/mall/battlepass/exp/buy", field_int(1, 43001) + field_int(2, 15), token)
    check("pass EXP is bought in tens", st == 400, f"status {st}")
    call("POST", "/admin/events", b"remove=2001", host="127.0.0.1")
    st, body = call("POST", "/g/mall/battlepass/exp/buy", field_int(1, 43001) + field_int(2, 10), token)
    r = decode("mall.BuyBattlePassExpResponse", body) if st == 200 else {}
    check("10 pass EXP cost 3 gems (pass_exp_price per slider step of 10)", st == 200 and qty(r.get("increaseItem", []), 20) == 260
          and qty(r.get("consumeGem", []), 3) == 997,
          f"{st}, {r}")
    call("POST", "/admin/events", b"id=4001", host="127.0.0.1")
    give(20002, 60)
    for _ in range(6):
        gacha(4001, 10)
    st, body = call("GET", "/g/gacha/step_reward/4001", token=token)
    r = decode("gacha.GachaStepRewardResponse", body) if st == 200 else {}
    st2, body2 = call("GET", "/g/gacha/step_reward/4001", token=token)
    r2 = decode("gacha.GachaStepRewardResponse", body2) if st2 == 200 else {}
    check("an event summon's step reward at 55 pulls (500 x item 12) is given once", st == 200
          and len(r.get("stepRewards", [])) == 1 and not r2.get("stepRewards"), f"{st} {r}, {st2} {r2}")
    call("POST", "/admin/events", b"remove=4001", host="127.0.0.1")

    # teams for the profile, element challenges and towers; account level from the admin page
    owned_ids = [c["id"] for c in snapshot()["charBag"]["chars"]]
    mine = [c for c in owned_ids if info.get(c, (0, 0))[1] != 1][:3]
    fire = next((c for c in owned_ids if info.get(c, (0, 0))[1] == 1), None)
    if fire:
        st, _ = call("POST", "/g/team/challenge", field_int(1, 4) + field_int(2, fire), token)
        check("a fire buddy cannot join the no-fire team (element 1)", st == 400, f"{fire}: status {st}")
    team = field_int(1, 4) + b"".join(field_int(2, c) for c in mine)
    st, _ = call("POST", "/g/team/challenge", team, token)
    st2, _ = call("POST", "/g/team/display", field_int(1, 1) + field_int(2, mine[0]), token)
    bag = snapshot()["charBag"]
    check("the no-fire team and the display team are saved", st == 200 and st2 == 200 and bag.get("banFire", {}).get("chars") == mine
          and bag.get("displayTeams", [{}])[0].get("chars") == mine[:1], f"{st}, {st2}, {bag.get('banFire')}, {bag.get('displayTeams')}")
    def multi(i, c):
        team = field_int(2, c)
        sets = varint(1 << 3 | 2) + varint(len(team)) + team + field_int(2, 2)
        return field_int(1, 0) + field_int(2, 2) + field_int(3, i) + varint(4 << 3 | 2) + varint(len(sets)) + sets
    st, _ = call("POST", "/g/team/multi", multi(0, mine[0]), token)
    st2, _ = call("POST", "/g/team/multi", multi(1, mine[1]), token)
    sets = snapshot()["charBag"].get("normalMultiTeam", [])
    check("a two-team tower's teams go into one set, in order", st == 200 and st2 == 200 and len(sets) == 1
          and [t.get("chars") for t in sets[0].get("teams", [])] == [mine[:1], mine[1:2]], f"{st}, {st2}, {sets}")
    call("POST", "/admin/level", f"account={acc_id}&lv=30".encode(), host="127.0.0.1")
    check("the admin page sets the account level", snapshot()["basic"].get("lv") == 30)
    st, body = call("POST", "/g/stage/", field_int(1, 273) + field_int(2, 27301) + field_int(3, 4), token)
    sn = decode("stage.DoStageResponse", body).get("stageSn", "") if st == 200 else ""
    res = field_int(5, 5)
    st2, body2 = call("POST", "/g/stage/over", field_str(1, sn) + varint(3 << 3 | 2) + varint(len(res)) + res, token)
    r = decode("stage.StageOverResponse", body2) if st2 == 200 else {}
    check("tower 273's first floor runs and gives its first-clear reward", st == 200 and st2 == 200 and r.get("firstClear")
          and r.get("rewardList"), f"{st}, {st2}, {r}")

    # two-team tower (chapter 250, 2 battles a stage): checkpoint after battle 1, the run goes on, a reset starts over
    st, body = call("POST", "/g/stage/", field_int(1, 250) + field_int(2, 25001) + field_int(3, 1), token)
    sn = decode("stage.DoStageResponse", body).get("stageSn", "") if st == 200 else ""
    res = field_int(5, 5)
    over = lambda s_: field_str(1, s_) + varint(3 << 3 | 2) + varint(len(res)) + res
    st2, body2 = call("POST", "/g/stage/multi_battle/checkpoint", over(sn), token)
    sn2 = decode("stage.DoStageResponse", body2).get("stageSn", "") if st2 == 200 else ""
    rec = stage_recs().get(25001, {})
    check("a two-team tower's first battle won: the run goes on (same number), the stage keeps battle 2 reached", st == 200
          and st2 == 200 and sn2 == sn and rec.get("multiBattleStatus", {}).get("phase") == 1, f"{st}, {st2}, {rec}")
    st, _ = call("POST", "/g/stage/multi_battle/checkpoint", over(sn2), token)
    check("the last battle does not checkpoint", st == 400, f"status {st}")
    st, body = call("POST", "/g/stage/over", over(sn2), token)
    r = decode("stage.StageOverResponse", body) if st == 200 else {}
    check("winning the last battle clears the stage and starts its record over", st == 200 and r.get("isWin")
          and not stage_recs().get(25001, {}).get("multiBattleStatus", {}).get("phase"), f"{st}, {r}")
    st, _ = call("POST", "/g/stage/multi_battle/reset", field_int(1, 25001), token)
    check("a two-team tower stage can be reset", st == 200, f"status {st}")

    # stores: GET g/store/{id}; the general store refreshes by hand (PriceDiscrim 5: 5 gems first), with new stock
    st, body = call("GET", "/g/store/1", token=token)
    stores = decode("store.GetStoreResponse", body).get("stores", []) if st == 200 else []
    check("the general store is answered with its 15 goods", st == 200 and len(stores) == 1 and len(stores[0].get("products", [])) == 15,
          f"status {st}, {len(stores)}")
    give(3, 1000)
    st, body = call("POST", "/g/store/refresh", field_int(1, 1) + field_int(3, 1), token)
    r = decode("store.StoreRefreshResponse", body) if st == 200 else {}
    prods = r.get("store", {}).get("products", [])
    check("a refresh by hand: 5 gems, 15 goods in stock again", st == 200 and r.get("refreshCount") == 1 and len(prods) == 15
          and all(p.get("quantity", 0) >= 1 for p in prods) and qty(r.get("remainingItem", []), 3) == 995, f"status {st}, {r.get('refreshCount')}")
    st, _ = call("POST", "/g/store/refresh", field_int(1, 3) + field_int(3, 1), token)
    check("a store without manual refreshes refuses one", st == 400, f"status {st}")
    st, body = call("GET", "/g/store/", token=token)
    every = decode("store.GetStoreResponse", body).get("stores", []) if st == 200 else []
    check("g/store/ (the client's store 0) answers every store", st == 200 and len(every) > 1
          and len({s["storeId"] for s in every}) == len(every), f"status {st}, {[s.get('storeId') for s in every]}")
    # GoodsAmount 0 is unlimited: store 6's group 61 goods (5 of item 32001 each)
    found = [(s["storeId"], p) for s in every for p in s.get("products", []) if p.get("groupId") == 61]
    if found:
        sid, p = found[0]
        give(32001, 1000)
        buy = lambda n: call("POST", "/g/store/buy", field_int(1, sid) + field_int(2, p.get("productId", 0)) + field_int(3, n), token)[0]
        st, st2 = buy(3), buy(4)
        check("an unlimited good (GoodsAmount 0) sells again and again", st == 200 and st2 == 200, f"{st}, {st2}")
    else:
        check("store 6 lists its unlimited goods (group 61)", False, f"{[s.get('storeId') for s in every]}")

    # gacha preview bonus (captured: gacha 48, buddy 10444, 5 gems once; the capture already has 10444), redeem codes,
    # sign-in make-up rules
    gems0 = qty(snapshot()["itemBag"]["items"], 3)
    st, body = call("POST", "/g/gacha/preview", field_int(1, 48) + field_int(2, 10445), token)
    r = decode("gacha.GachaPreviewResponse", body) if st == 200 else {}
    st2, body2 = call("POST", "/g/gacha/preview", field_int(1, 48) + field_int(2, 10445), token)
    r2 = decode("gacha.GachaPreviewResponse", body2) if st2 == 200 else {}
    check("a buddy's first preview gives 5 gems, the next nothing", qty(r.get("rewards", []), 3) == 5 and qty(r.get("lastItems", []), 3) == gems0 + 5
          and st2 == 200 and not r2.get("rewards"), f"{r}, {r2}")
    json.dump({"WOOFIA": [[3, 100]]}, open(os.path.join(DATA, "codes.json"), "w"))
    redeem = lambda c: call("POST", "/g/mall/vcex", field_str(1, c), token)
    st, body = redeem("woofia")
    r = decode("vc.VCExchangeResponse", body) if st == 200 else {}
    st2, body2 = redeem("WOOFIA")
    st3, body3 = redeem("NOPE")
    code_mail = [m for m in decode("mail.MailBox", call("GET", "/g/mailbox/0", token=token)[1]).get("mails", [])
                 if "Redeem Code" in m.get("mailText", {}).get("subject", "")]
    check("a redeem code mails its items once (0, then 3 = already claimed; 2 = invalid)", qty(r.get("rewards", []), 3) == 100
          and len(code_mail) == 1 and qty(code_mail[0].get("rewards", []), 3) == 100
          and str(r.get("resCode", 0)) == "0" and decode("vc.VCExchangeResponse", body2).get("resCode") == "3"
          and decode("vc.VCExchangeResponse", body3).get("resCode") == "2", f"{r}")
    st, _ = call("POST", "/g/signin/fill", field_int(1, 101), token)
    check("a sign-in without make-up days refuses one", st == 400, f"status {st}")

    # make-up (Welcome Party 1001, 10 gems, up to 3): the reply lists the days run so far (the client puts the
    # button on the first false entry) and the made-up day's rewards (its popup)
    # 1001 runs per player (the events page refuses it): move this account's run 3 days earlier, so days are missed
    st, body = call("POST", "/admin/newcomer-earlier", f"account={acc_id}&id=1001&days=3".encode(), host="127.0.0.1")
    check("the admin page moves a newcomer event's run earlier", b"day(s) earlier" in body, body[:200])
    call("GET", "/g/loginres", token=token)
    call("GET", "/g/signin", token=token)
    give(3, 100)
    st, body = call("POST", "/g/signin/fill", field_int(1, 1001), token)
    d = decode("signin.SignInFillResponse", body).get("data", {}) if st == 200 else {}
    days = d.get("isSignIn", [])
    rec = next((e for e in snapshot().get("event", {}).get("eventRecords", []) if e.get("id") == 1001), {})
    ran = (int(time.time()) - 75600) // 86400 - (int(rec.get("startTime", 0)) - 75600) // 86400 + 1
    check("a make-up day lists only the days run so far and pays the day's rewards", st == 200 and len(days) == min(ran, 10)
          and days[:2] == [True, True] and d.get("items"), f"status {st}, {ran} days run, {d}")

    # an event page's parts move with it (the page checks their own dates)
    st, body = call("POST", "/admin/events", b"id=10046", host="127.0.0.1")
    page = body.decode("utf-8", "replace") if isinstance(body, bytes) else str(body)
    check("scheduling Big Catch! Angler's Day moves its parts 10047-10049 with it", "[10047, 10048, 10049]" in page, page[:300])
    # its Market store (109) gets stock for the run (the account never had a record of it: the page was empty)
    st, body = call("GET", "/g/store/109", token=token)
    s109 = decode("store.GetStoreResponse", body).get("stores", []) if st == 200 else []
    goods = s109[0].get("products", []) if s109 else []
    check("a running event's Market store is stocked from its goods groups (store 109: 4 + 7)", len(goods) == 11
          and all(int(p.get("quantity", 0)) > 0 for p in goods), f"status {st}, {len(goods)} goods")
    call("POST", "/admin/events", b"remove=10046", host="127.0.0.1")

    # essence store: spare shards of a top-rank buddy turn into essence (item 12) by rarity
    top = next((c for c in snapshot()["charBag"]["chars"] if c.get("rank") == 75), None)
    if top:
        give(top["id"], 4)
        ess0 = qty(snapshot()["itemBag"]["items"], 12)
        st, body = call("POST", "/g/store/piece/change", b"", token)
        r = decode("store.PieceChangeResponse", body) if st == 200 else {}
        check("a top-rank buddy's 4 spare shards turn into essence", st == 200 and qty(r.get("pieces", []), top["id"]) == 4
              and r.get("serums", {}).get("id") == 12 and qty(snapshot()["itemBag"]["items"], 12) > ess0
              and qty(snapshot()["itemBag"]["items"], top["id"]) == 0, f"status {st}, {r}")

    # essence store records carry their own amount and price (captured store 4: 2 shards for 3000 essence)
    s4 = next((x for x in snapshot().get("store", {}).get("stores", []) if x["storeId"] == 4), {})
    p4 = next((p for p in s4.get("products", []) if p.get("quantity") and p.get("itemAmount")), None)
    if p4:
        give(12, 10000)
        have = qty(snapshot()["itemBag"]["items"], p4["itemId"])
        st, body = call("POST", "/g/store/buy", field_int(1, 4) + field_int(2, p4["productId"]) + field_int(3, 1), token)
        items = snapshot()["itemBag"]["items"]
        check("an essence store buy gives the record's amount for the record's price", st == 200
              and qty(items, 12) == 10000 - p4["price"] and qty(items, p4["itemId"]) == have + p4["itemAmount"], f"status {st}, {p4}")

    # challenge_schedule_C OpenDay: a weekday mask, Monday = bit 0, counted on the UTC+8 date the game day starts on
    # (05:00 UTC+8, so the UTC date 3 hours on). Chapter 201 opens on 4 days: try it on a shut day, then play on an
    # open day that is a new game day (its tries start over)
    give(4, 1000)
    open_days = {r["ChapterId"]: r["OpenDay"] for r in
                 json.load(open(os.path.join(TABLES, "challenge_schedule_C.json"), encoding="utf-8"))["rows"]}

    def weekday_in(days):
        return time.gmtime(time.time() + days * 86400 + 3 * 3600).tm_wday

    def clock_days(days):
        call("POST", "/admin/clock", b"reset=1", host="127.0.0.1")
        if days:
            call("POST", "/admin/clock", f"secs={days * 86400}".encode(), host="127.0.0.1")
    shut = next(d for d in range(7) if not open_days[201] >> weekday_in(d) & 1)
    clock_days(shut)
    st, _ = call("POST", "/g/stage/", field_int(1, 201) + field_int(2, 20101) + field_int(3, 1), token)
    check(f"a challenge chapter is shut on a day its OpenDay leaves out (weekday {weekday_in(shut)}, mask {open_days[201]})",
          st == 400, f"status {st}")
    clock_days(next(d for d in range(1, 8) if d != shut and open_days[201] >> weekday_in(d) & 1))

    # resource challenges: 3 tries a day shared by the type (challenge_times key 1), more for gems
    def ch_times(k):
        return snapshot().get("stage", {}).get("challengeTimes", {}).get(str(k), {})
    played0 = bought0 = 0
    ok_runs = 0
    for _ in range(10):
        st, body = call("POST", "/g/stage/", field_int(1, 201) + field_int(2, 20101) + field_int(3, 1), token)
        if st != 200:
            break
        sn = decode("stage.DoStageResponse", body)["stageSn"]
        res = field_int(5, 3)
        call("POST", "/g/stage/over", field_str(1, sn) + varint(3 << 3 | 2) + varint(len(res)) + res, token)
        ok_runs += 1
    check("resource challenges stop at the type's 3 tries a day (plus bought ones)", ok_runs == max(0, 3 + 3 * bought0 - played0)
          and ch_times(1).get("playedTimes") == played0 + ok_runs and "20101" not in snapshot()["stage"].get("challengeTimes", {}),
          f"{ok_runs} runs from {played0} played, {bought0} bought, {ch_times(1)}")
    # those clears count for the daily "Clear any Challenge 1 time" (mission 10006, condition 6)
    st, body = call("GET", "/g/mission?type=2", token=token)
    m10006 = {x["id"]: x for x in decode("mission.Mission", body).get("inProgress", [])}.get(10006, {}) if st == 200 else {}
    check("a challenge clear counts for the daily Clear any Challenge (10006)", ok_runs == 0 or m10006.get("value", 0) >= 1,
          f"{ok_runs} runs, {m10006}")
    # back to today: daily missions started over on the later day, so clear a stage today again (daily mission 10004)
    call("POST", "/admin/clock", b"reset=1", host="127.0.0.1")
    run(ch, three, good)

    # favourite buddies: Character.flag bit 1 (PinChar)
    fav = [c["id"] for c in snapshot()["charBag"]["chars"]][0]
    entry = field_int(1, fav) + field_int(2, 1)
    st, _ = call("POST", "/g/char/favorite", varint(1 << 3 | 2) + varint(len(entry)) + entry, token)
    pinned = int(char(fav).get("flag", 0)) & 1
    entry = field_int(1, fav)
    st2, _ = call("POST", "/g/char/favorite", varint(1 << 3 | 2) + varint(len(entry)) + entry, token)
    check("a buddy can be pinned and unpinned", st == 200 and pinned and st2 == 200 and not int(char(fav).get("flag", 0)) & 1, f"{st}, {st2}")

    # profile: home background, stickers, sticker plan
    bg = field_int(1, 7)
    st, _ = call("POST", "/g/userinfo/background", varint(5 << 3 | 2) + varint(len(bg)) + bg, token)
    sticker = field_int(1, 70001)
    give(70001, 1)
    st2, body2 = call("POST", "/g/userinfo/stickers", field_int(1, 2) + varint(2 << 3 | 2) + varint(len(sticker)) + sticker, token)
    st4, _ = call("POST", "/g/userinfo/stickers", field_int(1, 4) + varint(2 << 3 | 2) + varint(len(sticker)) + sticker, token)
    cs = snapshot()["basic"].get("charStickers", [])
    check("a home buddy's own stickers (where 3 + index) are saved", st4 == 200 and len(cs) >= 2
          and cs[1].get("stickers", [{}])[0].get("itemId") == 70001, f"{st4}, {cs}")
    st3, _ = call("POST", "/g/userinfo/sticker_plan", field_int(1, 1), token)
    b = snapshot()["basic"]
    check("the home background, profile stickers and sticker plan are saved", st == 200 and st2 == 200 and st3 == 200
          and b.get("homeBackgroundInfo", [{}])[0].get("id") == 7 and b.get("infoStickers", [{}])[0].get("itemId") == 70001
          and b.get("isHomeStickersCharSeperated"), f"{st}, {st2}, {st3}")

    # social, shared between accounts: follow, followers, feeds, likes, pokes, blocking, profile pages
    st, body = call("GET", "/g/social", token=token)
    me_social = decode("save.Social", body) if st == 200 else {}
    check("the own social save starts as captured (flag 5)", st == 200 and me_social.get("info", {}).get("flagInfo") == 5, f"{me_social}")
    st, body = call("GET", "/g/social", token=new_token)
    fresh_info = decode("save.Social", body).get("info", {}) if st == 200 else {}
    check("a new player's About Me starts as captured: Secret everywhere, MBTI 0 (bit flags; -1 shows as ESFP)",
          fresh_info.get("flagInfo") == 5 and fresh_info.get("age") == -1 and not fresh_info.get("flagMbti"), f"{fresh_info}")
    extra = field_int(1, 5) + field_int(2, 36) + field_int(4, 187)
    st, _ = call("POST", "/g/social/extra", extra, token)
    st2, body = call("GET", f"/g/social/extra/{acc_id}", token=new_token)
    r = decode("social.UserExtraInfoResponse", body) if st2 == 200 else {}
    check("profile details are not shown to a player who is not a mutual follower (code 3)", st == 200 and r.get("code") == 3
          and not r.get("info"), f"{r}")
    st, body = call("POST", "/g/userinfo/basic", field_int(1, acc_id), new_token)
    bi = decode("userinfo.UserBasicInfoResponse", body).get("infos", []) if st == 200 else []
    check("another player's basic info has profile flag bit 0 (the client won't follow a 0)", bi and bi[0].get("flagInfo", 0) & 1, f"{bi}")
    st, body = call("POST", "/g/social/follow", field_int(1, acc_id), new_token)
    r = decode("follow.SocialFollowResponse", body) if st == 200 else {}
    mask = newmask(call("GET", "/g/checknew", token=token)[1])
    check("a new follower lights the follower red dot (bit 2)", mask >> 2 & 1, f"{mask:b}")
    st0, body0 = call("POST", "/g/social/follow", field_int(1, acc_id), new_token)
    check("following again answers code 3", decode("follow.SocialFollowResponse", body0).get("code") == 3, f"{st0}")
    st2, body2 = call("GET", "/g/social/followers?index=0&asc=false", token=token)
    fl = decode("follow.SocialFollowersResponse", body2) if st2 == 200 else {}
    check("following a player (code 1) makes a new follower for them", r.get("code") == 1
          and [int(f["followerId"]) for f in fl.get("followers", [])] == [new_id] and fl["followers"][0].get("isNew")
          and fl["followers"][0].get("followData", {}).get("isFollowingMe"), f"{r}, {fl}")
    mask = newmask(call("GET", "/g/checknew", token=token)[1])
    check("opening the followers list clears the follower red dot", not mask >> 2 & 1, f"{mask:b}")
    st, body = call("GET", "/g/social/followings", token=new_token)
    fw = decode("follow.SocialFollowingsResponse", body).get("followings", []) if st == 200 else []
    check("the follower lists who they follow", [int(f["followingId"]) for f in fw] == [acc_id], f"{fw}")
    call("POST", "/g/social/follow", field_int(1, new_id), token)
    st, body = call("GET", f"/g/social/extra/{acc_id}", token=new_token)
    r = decode("social.UserExtraInfoResponse", body) if st == 200 else {}
    other = r.get("info", {})
    check("mutual followers see the profile details (code 1)", r.get("code") == 1 and other.get("age") == 36
          and other.get("height") == 187, f"{r}")
    call("POST", "/g/social/extra", field_int(1, 1) + field_int(2, 36) + field_int(4, 187), token)
    st, body = call("GET", f"/g/social/extra/{acc_id}", token=new_token)
    check("details hidden by their owner (flag bit 2 off) answer code 2", decode("social.UserExtraInfoResponse", body).get("code") == 2,
          f"status {st}")
    call("POST", "/g/social/extra", extra, token)
    call("POST", "/g/social/unfollow", field_int(1, new_id), token)
    st, body = call("POST", "/g/social/poke", field_int(1, acc_id), new_token)
    st2, body2 = call("POST", "/g/social/poke", field_int(1, acc_id), new_token)
    check("a poke once a day (then code 0)", decode("follow.SocialPokeResponse", body).get("code") == 1
          and decode("follow.SocialPokeResponse", body2).get("code", 0) == 0, f"{st}, {st2}")
    mask = newmask(call("GET", "/g/checknew", token=token)[1])
    check("a poke lights the follow news red dot (bit 1)", mask >> 1 & 1, f"{mask:b}")
    st, body = call("GET", "/g/social/following/feeds/0", token=token)
    feeds = decode("follow.SocialFollowingFeedsResponse", body).get("feeds", []) if st == 200 else []
    poke = next((f for f in feeds if f.get("type") == "POKE"), None)
    check("the poked player sees the poke in their following news, timed in milliseconds", poke is not None
          and int(poke["userId"]) == new_id and abs(int(poke.get("timestamp", 0)) - time.time() * 1000) < 86400 * 1000 * 30, f"{feeds[:3]}")
    mask = newmask(call("GET", "/g/checknew", token=token)[1])
    check("opening the news clears its red dot", not mask >> 1 & 1, f"{mask:b}")
    st, body = call("GET", "/g/social/my/feeds/0", token=token)
    mine = decode("follow.SocialMyFeedsResponse", body).get("feeds", []) if st == 200 else []
    check("my own feeds leave out pokes", all(f.get("type") != "POKE" for f in mine), f"{mine[:3]}")
    if poke:
        st, body = call("POST", "/g/social/like", field_str(1, poke["feedId"]), token)
        st2, body2 = call("GET", "/g/social/hotrank", token=token)
        st3, body3 = call("GET", "/g/social/recommend", token=token)
        check("follow recommendations answer", st3 == 200, f"status {st3}")
        check("a like counts (code 1)", decode("follow.SocialLikeResponse", body).get("code") == 1, f"{st}")
        ranks = {int(x.get("userId", 0)): x.get("hotness", 0) for x in decode("follow.SocialHotRankResponse", body2).get("ranks", [])}
        check("hotness is the likes and pokes received this week (the poked player and the liked poster rank)",
              ranks.get(acc_id, 0) >= 1 and ranks.get(new_id, 0) >= 1, f"{ranks}")
    st, body = call("GET", f"/g/userinfo/detail/{acc_id}", token=new_token)
    d = decode("userinfo.UserDetailInfo", body) if st == 200 else {}
    check("another player's profile page comes from their save, with their profile flags", st == 200
          and int(d.get("basicInfo", {}).get("userId", 0)) == acc_id and d.get("basicInfo", {}).get("flagInfo", 0) & 1
          and d.get("charNum", 0) > 0 and d.get("followData", {}).get("isFollowing"), f"status {st}, {d.get('basicInfo')}")
    st, _ = call("POST", "/g/social/blacklist/add", field_int(1, new_id), token)
    st2, body = call("GET", "/g/social/blacklist", token=token)
    bl = [int(x) for x in decode("follow.SocialBlacklistResponse", body).get("blacklist", [])] if st2 == 200 else []
    st3, body3 = call("POST", "/g/social/follow", field_int(1, acc_id), new_token)
    st4, body4 = call("GET", "/g/social/followings", token=new_token)
    check("blocking ends the follow and refuses a new one (code 4)", bl == [new_id]
          and decode("follow.SocialFollowResponse", body3).get("code") == 4
          and not decode("follow.SocialFollowingsResponse", body4).get("followings"), f"{bl}, {st3}")
    st, body = call("POST", "/g/social/poke", field_int(1, acc_id), new_token)
    check("a poke at a player not followed answers code 0", st == 200 and not decode("follow.SocialPokeResponse", body).get("code"),
          f"status {st}")
    call("POST", "/g/social/blacklist/remove", field_int(1, new_id), token)

    # event missions the client accepts at login (captured body: Bingo 17201, BBQ 19741 and 19742)
    accept = bytes.fromhex("0a099d9a019e9a01b18601")
    st, body = call("POST", "/g/mission/accept", accept, token)
    states = {x["id"]: x for x in decode("mission.MStates", body).get("states", [])} if st == 200 else {}
    next_reset = ((int(time.time()) - 75600) // 86400 + 1) * 86400 + 75600
    check("accepting the captured event missions: these three are daily (the next reset, as captured)",
          st == 200 and set(states) == {17201, 19741, 19742}
          and all(int(x.get("expiredUnix", 0)) == next_reset for x in states.values()), f"status {st}, {states}")
    st, body = call("GET", "/g/mission?type=5,25", token=token)
    listed = {x["id"] for x in decode("mission.Mission", body).get("inProgress", [])} if st == 200 else set()
    check("accepted event missions are listed", {17201, 19741, 19742} <= listed, f"{sorted(listed)[:20]}")

    # missions: daily, activity chests, archive bits (word << 48 | 48-bit map, mission m = bit m - 1)
    def missions(types):
        st, body = call("GET", "/g/mission?type=" + types, token=token)
        m = decode("mission.Mission", body) if st == 200 else {}
        ids = {s["id"]: s for s in m.get("inProgress", [])}
        done = set()
        for w in m.get("success", {}).get("archive", []):
            w = int(w)
            for b in range(48):
                if w >> b & 1:
                    done.add((w >> 48) * 48 + b + 1)
        return ids, done

    def claim(*ids):
        st, body = call("POST", "/g/mission/complete", b"".join(field_int(1, i) for i in ids), token)
        return st, decode("mission.MissionCompleteResponse", body) if st == 200 else {}
    prog, done = missions("2,6,3,7,1,10")
    check("the daily login mission is done and listed with its reset time", prog.get(10001, {}).get("value") == 1
          and int(prog[10001].get("expiredUnix", 0)) > 0 and 10001 not in done, f"{prog.get(10001)}")
    check("milestones show the next mission of their chain", all(t not in prog for t in done if t < 10000), "a claimed milestone is listed")
    st, r = claim(10001)
    check("claiming the daily login gives 1000 gold and 15 activity points", st == 200 and qty(r.get("rewards", []), 1) == 1000
          and qty(r.get("rewards", []), 8) == 15, f"status {st}, {r.get('rewards')}")
    st, _ = claim(10001)
    check("a mission is claimed once a day", st == 400, f"status {st}")
    st, _ = claim(10008)
    check("an unfinished mission cannot be claimed", st == 400 or prog.get(10008, {}).get("value", 0) >= 1, f"status {st}")
    st, r = claim(10004)
    check("the stage clear daily counts the stages played today", st == 200, f"status {st}, {prog.get(10004)}")
    st, r = claim(10501)
    check("the first daily chest opens at 20 activity points", st == 200, f"status {st}")
    prog, done = missions("2,6,3,7,1,10")
    check("claimed missions are in the archive and leave the list", {10001, 10004, 10501} <= done and 10001 not in prog, f"{sorted(done)[:12]}")
    timed = {i["id"]: i["set"] for i in snapshot()["itemBag"]["items"] if i["id"] in (8, 9)}
    check("activity points keep their expiry time (the client stops loading without it)",
          len(timed) == 2 and all(int(st0.get("unix", 0)) > 0 for v in timed.values() for st0 in v), f"{timed}")
    defs = {m["MissionId"]: m for m in json.load(open(os.path.join(TABLES, "Mission_C.json"), encoding="utf-8"))["rows"]}

    def claimable(prog):
        return [i for i, x in prog.items() if defs.get(i, {}).get("Type") == 1 and x.get("value", 0) >= defs[i]["FinishValue"]]
    prog, _ = missions("2,6,3,7,1,10")
    ready = claimable(prog)
    st, r = claim(*ready) if len(ready) > 1 else (200, {})
    prog, _ = missions("2,6,3,7,1,10")
    later = [i for i in claimable(prog) if defs.get(i, {}).get("BeforeMissionId") in ready]
    check("Claim All claims the ids sent, no more (captured: a done next step stays listed)", st == 200
          and not set(ready) & set(claimable(prog)) and set(claimable(prog)) == set(later),
          f"{len(ready)} claimed, still ready: {claimable(prog)[:8]}")
    ev, _ = missions("10,5,23,25")
    check("event missions are listed from the captures", len(ev) > 0 and all(i >= 10000 for i in ev), f"{len(ev)}")

    # memories: the replayed list starts from the capture and grows with each recall
    st, body = call("GET", "/g/memory/avg_rec_list", token=token)
    before = decode("memory.GetAvgRecListResponse", body).get("avgList", []) if st == 200 else None
    st, body = call("POST", "/g/memory/recall", field_int(1, 14141), token)
    st2, body = call("GET", "/g/memory/avg_rec_list", token=token)
    after = decode("memory.GetAvgRecListResponse", body).get("avgList", [])
    check("a recalled memory is added to the replayed list once", st == 200 and before is not None and after == before + [14141]
          and call("POST", "/g/memory/recall", field_int(1, 14141), token)[0] == 200
          and decode("memory.GetAvgRecListResponse", call("GET", "/g/memory/avg_rec_list", token=token)[1]).get("avgList") == after,
          f"{before} -> {after}")
    st, body = call("POST", "/g/memory/recall", field_int(2, 10101), token)
    check("a stage's story replayed from the stage list answers code 1 and is not listed", st == 200
          and decode("memory.MemoryRecallResponse", body).get("code") == 1
          and decode("memory.GetAvgRecListResponse", call("GET", "/g/memory/avg_rec_list", token=token)[1]).get("avgList") == after,
          f"status {st}")

    had = any(x["id"] == 10414 for x in snapshot()["charBag"]["chars"])
    give(40106, 1)
    use = field_int(1, 40106) + varint(2 << 3 | 2) + varint(len(field_int(1, 1))) + field_int(1, 1)
    st, body = call("POST", "/g/item/consume_char_item", varint(1 << 3 | 2) + varint(len(use)) + use + field_int(2, 10414), token)
    r = decode("item.ConsumeCharItemResponse", body) if st == 200 else {}
    has = any(x["id"] == 10414 for x in snapshot()["charBag"]["chars"])
    check("the buddy card gives Kumoyama (or shards if owned)", st == 200 and has and (bool(r.get("reward")) == had), f"status {st}, {r}")
    give(40106, 1)
    st, _ = call("POST", "/g/item/consume_char_item", varint(1 << 3 | 2) + varint(len(use)) + use + field_int(2, 10001), token)
    check("the buddy card refuses a character it does not hold", st == 400, f"status {st}")

    # event schedule and sign-in days, played out on the test clock (WOOFIA_TEST_CLOCK): a sign-in rerun waits for
    # the 21:00 UTC reset, the CDN serves main_data with the new dates, and every login reply is like the official
    # ones: sign-ins paid out together (items, hasFirst false) or none (hasFirst true), never a mix
    def signin_reply(tok):
        st, body = call("GET", "/g/signin", token=tok)
        return decode("signin.SignInResponse", body).get("datas", []) if st == 200 else []

    def unmixed(datas):
        paid = [d["id"] for d in datas if d.get("items")]
        return (not paid or all(d.get("items") and not d.get("hasFirst") for d in datas if d.get("isSignedToday"))) \
            and all(d.get("hasFirst") for d in datas if not d.get("items"))

    st, body = call("POST", "/admin/clock", b"secs=0", host="127.0.0.1")
    check("the test instance runs with a test clock (start it with WOOFIA_TEST_CLOCK=1)", st == 200, f"status {st}")
    CDN = "assets.woofia.playhorny.com"
    cat = "/config/catalog/Android/data_BD202610061503"
    orig_hash = call("GET", cat + ".hash", host=CDN)[1]
    st, body = call("POST", "/admin/events", b"id=1038", host="127.0.0.1")
    check("the admin page schedules a past event", st == 200 and b"main_data rebuilt" in body, body[:300])
    new_hash = call("GET", cat + ".hash", host=CDN)[1]
    info = call("GET", cat + ".info", host=CDN)[1].decode()
    name = next((l.split("|")[0] for l in info.splitlines() if l.startswith("main_data_assets_all_")), "")
    st, bundle = call("GET", "/bundle/Android/" + name, host=CDN)
    check("the CDN serves the patched catalog and its main_data", new_hash != orig_hash and st == 200
          and str(len(bundle)) == next(l.split("|")[1] for l in info.splitlines() if l.startswith(name)), f"{name}, status {st}")
    now = int(time.time())
    reset = ((now - 75600) // 86400 + 1) * 86400 + 75600
    lr = decode("save.LoginRes", call("GET", "/g/loginres", token=new_token)[1])
    rec = next((r for r in lr["event"]["eventRecords"] if r["id"] == 1038), {})
    datas = signin_reply(new_token)
    check("a sign-in rerun without a date starts at the next 21:00 UTC reset, not claimable before it",
          int(rec.get("startTime", 0)) == reset and rec.get("nthDay", 0) == 0 and not lr.get("canGetSignInReward")
          and all(d["id"] != 1038 for d in datas) and not any(d.get("items") for d in datas), f"{rec}, {[d['id'] for d in datas]}")
    before = {d["id"]: len([x for x in d.get("isSignIn", []) if x]) for d in datas}

    call("POST", "/admin/clock", f"secs={reset - now + 60}".encode(), host="127.0.0.1")
    lr = decode("save.LoginRes", call("GET", "/g/loginres", token=new_token)[1])
    datas = signin_reply(new_token)
    after = {d["id"]: len([x for x in d.get("isSignIn", []) if x]) for d in datas}
    check("after the reset the login has sign-ins waiting", lr.get("canGetSignInReward") is True, f"{lr.get('canGetSignInReward')}")
    check("at the reset every sign-in pays its next day together, the rerun its day 1 (no mixed reply)", unmixed(datas)
          and after.get(1038) == 1 and all(after.get(i, 0) == n + 1 for i, n in before.items() if n < 5)
          and all(d.get("items") for d in datas), f"{before} -> {after}, {[(d['id'], bool(d.get('items')), d.get('hasFirst')) for d in datas]}")
    rec = next((r for r in lr["event"]["eventRecords"] if r["id"] == 1001), {})
    check("event records count the run's day (Welcome Party day 2)", rec.get("nthDay") == 2, f"{rec}")
    lr = decode("save.LoginRes", call("GET", "/g/loginres", token=new_token)[1])
    datas = signin_reply(new_token)
    check("a second login that day: nothing waiting, nothing paid, all hasFirst", not lr.get("canGetSignInReward")
          and unmixed(datas) and not any(d.get("items") for d in datas), f"{[(d['id'], d.get('hasFirst')) for d in datas]}")
    call("POST", "/admin/clock", b"reset=1", host="127.0.0.1")
    st, body = call("POST", "/admin/events", b"clear=1", host="127.0.0.1")
    check("clearing the schedule serves the original catalog again", st == 200 and call("GET", cat + ".hash", host=CDN)[1] == orig_hash)

    # past event mini-games, rerun from the schedule: the Crocodile Diner vote (10056 page, 10058 voting) and the
    # Find Character run 10014 (stages 1001401-05 open with 10014, 06-10 with 10015)
    for ev in (10056, 10058, 10014, 10015):
        call("POST", "/admin/events", f"id={ev}".encode(), host="127.0.0.1")
    vote = field_int(1, 10058) + field_int(2, 1) + field_int(3, 10402) + field_int(4, 2)
    st, _ = call("POST", "/g/event/vote", vote, token)
    check("voting needs the condition stage (1003325) cleared", st == 400, f"status {st}")
    call("POST", "/admin/story", f"account={acc_id}&stage=1003325".encode(), host="127.0.0.1")
    call("POST", "/admin/item", f"account={acc_id}&id=129&qty=2500".encode(), host="127.0.0.1")
    ink = qty(snapshot()["itemBag"]["items"], 30012)
    st, body = call("POST", "/g/event/vote", vote, token)
    r = decode("activity.VoteResponse", body) if st == 200 else {}
    check("2 votes cost 2000 Vote Cards and give 40 Sigil Ink", st == 200 and qty(r.get("gainItems", []), 30012) == 40
          and qty(r.get("lastItems", []), 129) == 500 and qty(r.get("lastItems", []), 30012) == ink + 40, f"status {st}, {r}")
    st, _ = call("POST", "/g/event/vote", vote, token)
    check("a vote without enough Vote Cards is refused", st == 400, f"status {st}")
    st, _ = call("POST", "/g/event/vote", field_int(1, 10058) + field_int(2, 1) + field_int(3, 10441) + field_int(4, 1), token)
    check("a character outside the vote group is refused", st == 400, f"status {st}")
    st, body = call("GET", "/g/event/vote/rec?event_id=10056,10058", token=token)
    recs = decode("activity.GetVoteRecResponse", body).get("voteEventRecs", []) if st == 200 else []
    check("the player's votes, under either event id", [(x["eventId"], x.get("totalVote")) for x in recs] == [(10056, 2), (10058, 2)]
          and recs[0]["voteGroupRecs"][0]["voteRecs"] == [{"id": 10402, "count": 2}], f"{recs}")
    call("POST", "/admin/story", f"account={new_id}&stage=1003325".encode(), host="127.0.0.1")
    call("POST", "/admin/item", f"account={new_id}&id=129&qty=1000".encode(), host="127.0.0.1")
    call("POST", "/g/event/vote", vote[:-2] + field_int(4, 1), new_token)
    st, body = call("GET", "/g/event/vote/ranking?event_id=10056", token=token)
    rank = decode("activity.GetRankingResponse", body).get("voteGroupRecs", []) if st == 200 else []
    check("the ranking adds up every account's votes", rank and rank[0].get("voteRecs") == [{"id": 10402, "count": 3}], f"{rank}")

    st, body = call("GET", "/g/event/find_char/10014", token=token)
    fc = decode("activity.FindCharEvent", body) if st == 200 else None
    check("a new Find Character run has no stars", fc is not None and int(fc.get("id", 0)) == 10014 and not fc.get("miniGameRecs"), f"{fc}")
    gems = qty(snapshot()["itemBag"]["items"], 3)
    st, body = call("POST", "/g/event/find_char/complete", field_int(1, 1001401) + field_int(2, 5) + field_int(3, 10014), token)
    r = decode("activity.FindCharRewardResponse", body) if st == 200 else {}
    st2, body2 = call("POST", "/g/event/find_char/complete", field_int(1, 1001401) + field_int(2, 3) + field_int(3, 10014), token)
    r2 = decode("activity.FindCharRewardResponse", body2) if st2 == 200 else {}
    check("a stage's first win pays its drop (5 gems), a replay pays nothing", qty(r.get("gainItems", []), 3) == 5
          and qty(r.get("lastItems", []), 3) == gems + 5 and st2 == 200 and not r2.get("gainItems"), f"{r}, {r2}")
    st, _ = call("POST", "/g/event/find_char/star_reward", field_int(1, 10014) + field_int(2, 0), token)
    check("a star box needs its stars (15)", st == 400, f"status {st}")
    for sid, n in ((1001402, 4), (1001403, 5), (1001404, 5), (1001405, 5)):
        call("POST", "/g/event/find_char/complete", field_int(1, sid) + field_int(2, n) + field_int(3, 10014), token)
    st, body = call("POST", "/g/event/find_char/star_reward", field_int(1, 10014) + field_int(2, 0), token)
    r = decode("activity.FindCharRewardResponse", body) if st == 200 else {}
    st2, _ = call("POST", "/g/event/find_char/star_reward", field_int(1, 10014) + field_int(2, 0), token)
    st3, body3 = call("GET", "/g/event/find_char/10014", token=token)
    fc = decode("activity.FindCharEvent", body3) if st3 == 200 else {}
    check("star box 1 (15 stars) pays 3 tickets once, the record keeps stars and the box", st == 200
          and qty(r.get("gainItems", []), 20001) == 3 and st2 == 400 and fc.get("starReward") == 1
          and sum(x.get("star", 0) for x in fc.get("miniGameRecs", [])) == 24, f"status {st}, {r}, {fc}")
    call("POST", "/admin/events", b"clear=1", host="127.0.0.1")

    # lotteries of past events, rerun from the schedule: the wheel 7001 (50 draws, step rewards at 10/20/30/40/50)
    # and the poking boards of event 10031 (boards 111-130, the next one after the target is found)
    for ev in (7001, 10031):
        call("POST", "/admin/events", f"id={ev}".encode(), host="127.0.0.1")
    call("POST", "/admin/item", f"account={acc_id}&id=20004&qty=60".encode(), host="127.0.0.1")
    ten = field_int(1, 7001) + field_int(2, 10)
    st, body = call("POST", "/g/event/lottery/poke", ten, token)
    r = decode("activity.PokeLotteryResponse", body) if st == 200 else {}
    check("a wheel ten-draw costs 10 tickets and lists 10 draws, one record each", st == 200
          and len(r.get("records", [])) == 10 and all(x.get("times") == 1 for x in r.get("records", []))
          and qty(r.get("lastItems", []), 20004) == 50 + qty(r.get("gainItems", []), 20004), f"status {st}, {r}")
    st, body = call("GET", "/g/event/lottery?id=7001", token=token)
    rec = decode("activity.GetLotteryRecResponse", body).get("lotteryRecs", [{}])[0] if st == 200 else {}
    check("the wheel record lists the 10 draws and the step reached, none claimed", len(rec.get("records", [])) == 10
          and rec.get("lineConnected") == 1 and not rec.get("lineNumber") and not rec.get("lineRewarded"), f"{rec}")
    st, body = call("GET", "/g/event/lottery/step_reward/7001", token=token)
    r = decode("activity.LotteryStepRewardResponse", body) if st == 200 else {}
    st2, _ = call("GET", "/g/event/lottery/step_reward/7001", token=token)
    check("the 10-draw step pays 20 gems once", st == 200 and qty(r.get("gainItems", []), 3) == 20 and st2 == 400, f"status {st}, {r}, {st2}")
    for _ in range(4):
        call("POST", "/g/event/lottery/poke", ten, token)
    st, _ = call("POST", "/g/event/lottery/poke", field_int(1, 7001) + field_int(2, 1), token)
    st2, body = call("GET", "/g/event/lottery?id=7001", token=token)
    rec = decode("activity.GetLotteryRecResponse", body).get("lotteryRecs", [{}])[0] if st2 == 200 else {}
    won = {}
    for x in rec.get("records", []):
        won[x["rewardId"]] = won.get(x["rewardId"], 0) + 1
    check("50 draws take the whole stock, the costume (reward 3) included, then the wheel stops",
          st == 400 and sum(won.values()) == 50 and won.get(3) == 1 and len(won) == 18 and rec.get("lineConnected") == 31, f"status {st}, {won}")
    st, body = call("GET", "/g/event/lottery/step_reward/7001", token=token)
    r = decode("activity.LotteryStepRewardResponse", body) if st == 200 else {}
    check("the other four steps pay together", st == 200 and qty(r.get("gainItems", []), 20004) == 2
          and qty(r.get("gainItems", []), 20002) == 3, f"status {st}, {r}")
    st, body = call("GET", "/g/event/lottery?id=7001", token=token)
    rec = decode("activity.GetLotteryRecResponse", body).get("lotteryRecs", [{}])[0] if st == 200 else {}
    check("with all five steps claimed the record's line number is 5", rec.get("lineNumber") == 5, f"{rec.get('lineNumber')}")

    call("POST", "/admin/item", f"account={acc_id}&id=113&qty=2400".encode(), host="127.0.0.1")
    boards = ",".join(str(i) for i in range(111, 131))
    listed = lambda q=boards: [(x.get("lotteryId"), bool(x.get("skippable"))) for x in decode("activity.GetLotteryRecResponse", call("GET", f"/g/event/lottery?id={q}", token=token)[1]).get("lotteryRecs", [])]
    first = listed()
    check("every poking board is listed in order, none passed yet (the client opens on the first not passed)",
          [i for i, _ in first] == list(range(111, 131)) and not any(p for _, p in first), f"{first}")
    st, _ = call("POST", "/g/event/lottery/poke", field_int(1, 112) + field_int(3, 0), token)
    check("the second poking board stays locked until the first is done", st == 400, f"status {st}")
    st, body = call("POST", "/g/event/lottery/poke", field_int(1, 111) + field_int(3, 5), token)
    r = decode("activity.PokeLotteryResponse", body) if st == 200 else {}
    first_hit = r.get("records", [{}])[0].get("rewardId") == 1 if r.get("records") else False
    check("a poke costs 50 of item 113, draws a reward and gives a coin (114)", st == 200 and len(r.get("records", [])) == 1
          and r["records"][0].get("position") == 5 and qty(r.get("gainItems", []), 114) == 1
          and qty(r.get("lastItems", []), 113) == 2350, f"status {st}, {r}")
    st, body = call("POST", "/g/event/lottery/autopoke", field_int(1, 111), token)
    r = decode("activity.AutoPokeLotteryResponse", body) if st == 200 else {}
    recs = r.get("records", [])
    # 1 in 24: the poke above found it, and autopoke then opens the rest of the board
    check("autopoke stops when the target (reward 1) turns up", st == 200 and recs and (
          (len(recs) == 23 and not any(x.get("rewardId") == 1 for x in recs)) if first_hit else
          (recs[-1].get("rewardId") == 1 and sum(1 for x in recs if x.get("rewardId") == 1) == 1)),
          f"status {st}, first poke hit {first_hit}, {[x.get('rewardId') for x in recs]}")
    st, _ = call("POST", "/g/event/lottery/poke", field_int(1, 112) + field_int(3, 0), token)
    check("with the target found the next board opens", st == 200, f"status {st}")
    now = listed("111")
    check("with its target found the board reads as passed (the client's next button moves on)", now == [(111, True)], f"{now}")
    call("POST", "/admin/events", b"clear=1", host="127.0.0.1")

    # mission conditions: 28 (a mall purchase not paid with gems, the Bingo daily task), 214 (Bond Add-Up), 108 (a stage
    # cleared with 3 stars), 12 (event items gained), and the daily event missions
    def accept(tok, mid):
        st, body = call("POST", "/g/mission/accept", field_int(1, mid), tok)
        return decode("mission.MStates", body).get("states", [{}])[0] if st == 200 else {}

    def in_progress(tok, types, mid):
        st, body = call("GET", f"/g/mission?type={types}", token=tok)
        return next((x for x in decode("mission.Mission", body).get("inProgress", []) if x["id"] == mid), {}) if st == 200 else {}

    reset = ((int(time.time()) - 75600) // 86400 + 1) * 86400 + 75600
    call("POST", "/admin/hcoin", f"account={acc_id}&qty=1000".encode(), host="127.0.0.1")
    call("POST", "/g/mall/cash/purchase", field_int(2, 51011), token)
    task = accept(token, 17201)
    check("the Bingo daily task counts today's H Coin purchases and ends at the next reset", task.get("value", 0) >= 1
          and int(task.get("expiredUnix", 0)) == reset, f"{task}")
    bond = in_progress(token, "3", 11015)
    check("Bond Add-Up presses (room collects) count for \"Add up Bond 30 times\" this week (condition 214)", bond.get("value", 0) >= 1, f"{bond}")
    st, _ = call("POST", "/g/mission/complete", field_int(1, 17201), token)
    call("POST", "/admin/clock", f"secs={reset - int(time.time()) + 60}".encode(), host="127.0.0.1")
    task = accept(token, 17201)
    st2, _ = call("POST", "/g/mission/complete", field_int(1, 17201), token)
    check("a daily event mission starts over the next day (claimed, then 0 and claimable again after a purchase)", st == 200
          and task.get("value") == 0 and int(task.get("expiredUnix", 0)) == reset + 86400 and st2 == 400, f"{task}, {st}, {st2}")
    call("POST", "/admin/clock", b"reset=1", host="127.0.0.1")
    three = accept(token, 19724)
    call("POST", "/admin/story", f"account={acc_id}&stage=1004428".encode(), host="127.0.0.1")
    three_after = in_progress(token, "5", 19724)
    check("condition 108 needs the stage cleared with 3 stars", three.get("value") == 0 and three_after.get("value") == 1,
          f"{three}, {three_after}")
    call("POST", "/admin/events", b"id=10046", host="127.0.0.1")
    cond12 = next((mid for mid in range(18900, 19100) if decode_mission_cond(mid) == (12, 122)), None)
    if cond12:
        accept(token, cond12)
    call("POST", "/admin/item", f"account={acc_id}&id=123&qty=100".encode(), host="127.0.0.1")
    for cell in range(3):
        call("POST", "/g/event/lottery/poke", field_int(1, 131) + field_int(3, cell), token)
    got = in_progress(token, "5", cond12) if cond12 else {}
    check("condition 12 counts event items gained (3 pokes, 3 coins 122)", cond12 and got.get("value") == 3, f"mission {cond12}, {got}")
    call("POST", "/admin/events", b"clear=1", host="127.0.0.1")

    # event missions start over when their event runs again: Star Level Up 3104 (mission 17019: own buddy 10443)
    def archived(mission_body, mid):
        words = [int(w) for w in decode("mission.Mission", mission_body).get("success", {}).get("archive", [])]
        bit = mid - 1
        return any(w >> 48 == bit // 48 and (w >> (bit % 48)) & 1 for w in words)

    # the client never accepts Star Level Up missions (type 19): it lists them (g/mission?type=19) and claims
    def listed(mid):
        body = call("GET", "/g/mission?type=19", token=token)[1]
        return next((x for x in decode("mission.Mission", body).get("inProgress", []) if x.get("id") == mid), {}), body

    call("POST", "/admin/all-summons", f"account={acc_id}".encode(), host="127.0.0.1")
    call("POST", "/admin/events", b"id=3104", host="127.0.0.1")
    first, _ = listed(17019)
    st2, _ = call("POST", "/g/mission/complete", field_int(1, 17019), token)
    claimed_a = archived(call("GET", "/g/mission?type=19", token=token)[1], 17019)
    check("Star Level Up run A: mission 17019 counts without an accept (buddy owned: 1 of 1) and is claimed",
          int(first.get("value", 0)) == 1 and st2 == 200 and claimed_a, f"{first}, claim {st2}, archived {claimed_a}")
    call("POST", "/admin/clock", f"secs={20 * 86400}".encode(), host="127.0.0.1")
    call("POST", "/admin/events", b"id=3104", host="127.0.0.1")
    again, body = listed(17019)
    claimed_b = archived(body, 17019)
    st2, _ = call("POST", "/g/mission/complete", field_int(1, 17019), token)
    check("the rerun (run B) starts 17019 over: unclaimed, listed again, claimable again", not claimed_b
          and int(again.get("value", 0)) == 1 and st2 == 200, f"{again}, archived {claimed_b}, claim {st2}")
    call("POST", "/admin/clock", b"reset=1", host="127.0.0.1")
    call("POST", "/admin/events", b"clear=1", host="127.0.0.1")

    # link codes: the main account makes a code, the third account's device uses it and plays the main account
    st, body = call("GET", "/g/acc/linkcode", token=token)
    check("no link code to start with", st == 200 and not decode("login.GenLinkCodeResponse", body).get("linkCode"), f"status {st}")
    st0, body0 = call("POST", "/g/acc/linkcode", field_str(1, "pw12"), token)
    check("a link code password needs 8 letters or digits (res_code 1)", decode("login.GenLinkCodeResponse", body0).get("resCode") == 1,
          f"status {st0}")
    st, body = call("POST", "/g/acc/linkcode", field_str(1, "pw123456"), token)
    made = decode("login.GenLinkCodeResponse", body) if st == 200 else {}
    st2, body2 = call("GET", "/g/acc/linkcode", token=token)
    shown = decode("login.GenLinkCodeResponse", body2) if st2 == 200 else {}
    code = made.get("linkCode", "")
    check("a link code is made for a day and shown again", len(code) == 10 and 0 < int(made.get("exp", 0)) - time.time() <= 86400 + 60
          and shown.get("linkCode") == code, f"{made}, {shown}")
    third_game, third_ph = ph_login("smoke-third@woofia.test", "smoke-third-pass1")
    third_name = f"{third_game}@horny"
    st, body = call("POST", "/acc/linkcode/auth", field_str(1, code) + field_str(2, "wrong") + field_str(4, third_name))
    check("a wrong link code password answers verification failed (res_code 1)", st == 200
          and decode("login.LinkCodeResponse", body).get("accResponse", {}).get("resCode") == 1, f"status {st}")
    # the client's rule: an account that has reached LV.10 (or bought with H Coin) is not overwritten (res_code 4)
    call("POST", "/admin/level", f"account={third_id}&lv=10".encode(), host="127.0.0.1")
    st, body = call("POST", "/acc/linkcode/auth", field_str(1, code) + field_str(2, "pw123456") + field_str(4, third_name))
    kept = decode("login.LinkCodeResponse", body) if st == 200 else {}
    check("a device whose account has reached LV.10 keeps it (res_code 4, with both accounts' cards for the panel)",
          kept.get("accResponse", {}).get("resCode") == 4 and int(kept.get("nowAcc", {}).get("userId", 0)) == third_id
          and int(kept.get("oriAcc", {}).get("userId", 0)) == acc_id, f"status {st}, {kept}")
    call("POST", "/admin/level", f"account={third_id}&lv=9".encode(), host="127.0.0.1")
    st, body = call("POST", "/acc/linkcode/auth", field_str(1, code) + field_str(2, "pw123456") + field_str(4, third_name))
    lr = decode("login.LinkCodeResponse", body) if st == 200 else {}
    ar = lr.get("accResponse", {})
    check("the link code answers a login for the main account, with both accounts' info", int(ar.get("id", 0)) == acc_id
          and int(lr.get("oriAcc", {}).get("userId", 0)) == acc_id and int(lr.get("nowAcc", {}).get("userId", 0)) == third_id,
          f"status {st}, {lr}")
    acc_bytes = acc_info(third_game, third_ph)
    bind = field_str(1, ar.get("loginToken", "")) + varint(2 << 3 | 2) + varint(len(acc_bytes)) + acc_bytes + field_int(3, acc_id)
    st, _ = call("POST", "/acc/binding", bind)
    moved_id, _ = login(acc_info(third_game, third_ph))
    st2, body2 = call("POST", "/acc/linkcode/auth", field_str(1, code) + field_str(2, "pw123456") + field_str(4, third_name))
    check("binding moves the third device to the main account, and the code is used up", st == 200 and moved_id == acc_id
          and decode("login.LinkCodeResponse", body2).get("accResponse", {}).get("resCode") == 1, f"bind {st}, now {moved_id}, reuse {st2}")
    st, body = call("GET", "/server_time")
    stm = decode("model.ServerTime", body) if st == 200 else {}
    nxt = int(stm.get("nextCrossdayUnix", 0))
    check("server time is the server's own, with the next 21:00 UTC reset", abs(int(stm.get("unixTime", 0)) - time.time()) < 120
          and nxt % 86400 == 75600 and 0 < nxt - int(stm.get("unixTime", 0)) <= 86400, f"{stm}")
    st, body = call("GET", "/auth/refresh_token", token=token)
    rt = decode("login.RefreshResponse", body) if st == 200 else {}
    check("a session token can be renewed", rt.get("token", "").startswith(f"wf.{acc_id}.") and int(rt.get("expire", 0)) > time.time(),
          f"status {st}")

    # the summon page shows FREE from the login's counts: they start over before any pull of the day (last, as the
    # clock jump renews the day's boards)
    call("POST", "/g/gacha", field_int(1, 12) + field_int(2, 1) + field_int(5, 1), token)
    used = next(x for x in snapshot()["mall"]["gacha"]["gachaRecs"] if x["id"] == 12).get("freeCnt", 0)
    call("POST", "/admin/clock", f"secs={2 * 86400}".encode(), host="127.0.0.1")
    fc = next(x for x in snapshot()["mall"]["gacha"]["gachaRecs"] if x["id"] == 12).get("freeCnt", 0)
    call("POST", "/admin/clock", b"reset=1", host="127.0.0.1")
    check("the next day's login shows the standard banner's free pull unused", used == 1 and fc == 0, f"freeCnt {used} -> {fc}")

    # the admin page issues a session token for an account (test scripts act as a second player with it)
    st, tok = call("POST", "/admin/token", f"account={acc_id}".encode(), host="127.0.0.1")
    st2, _ = call("GET", "/g/energy", token=tok.decode()) if st == 200 else (0, b"")
    st3, _ = call("POST", "/admin/token", b"account=1", host="127.0.0.1")
    check("an admin session token works for its account; an unknown account gets none", st == 200 and st2 == 200 and st3 == 404,
          f"{st}, {st2}, {st3}")
    st4, body4 = call("POST", "/admin/token", f"account={acc_id}".encode(), host=GAME)
    check("the token action is not reachable through the game host", not body4.startswith(b"wf."), f"status {st4}, {body4[:20]}")

    # Wishing Pool: at most 4 stars a player a day; all three colors at the level's LvStarAmount (150 at level 1)
    # raise the level and the stars start over. A fresh fourth player founds a guild and donates day after day.
    call("POST", "/admin/login", b"name=smoke-pool@woofia.test&password=smoke-pool-pass1", host="127.0.0.1")
    pool_id, pool_token = login(acc_info(*ph_login("smoke-pool@woofia.test", "smoke-pool-pass1")))
    for item, n in ((3, 1000), (14, 200), (15, 200), (16, 200)):
        call("POST", "/admin/item", f"account={pool_id}&id={item}&qty={n}".encode(), host="127.0.0.1")
    call("POST", "/g/guild/new", field_str(1, "PoolTest"), pool_token)
    stars = lambda a, b, c: varint(1 << 3 | 2) + varint(len(varint(a) + varint(b) + varint(c))) + varint(a) + varint(b) + varint(c)
    st0, _ = call("POST", "/g/guild/pool/donate", stars(4, 0, 0), pool_token)
    codes, lv = [], 1
    for day in range(1, 120):
        call("POST", "/admin/clock", b"secs=86400", host="127.0.0.1")
        colour = [0, 0, 0]
        colour[(day - 1) // 38 % 3] = 4
        st, body = call("POST", "/g/guild/pool/donate", stars(*colour), pool_token)
        r = decode("guild.GuildPoolDonateResponse", body) if st == 200 else {}
        codes.append(r.get("code", st))
        lv = int(r.get("pool", {}).get("lv", lv) or lv)
        if day == 1:
            st2, body2 = call("POST", "/g/guild/pool/donate", stars(1, 0, 0), pool_token)
            over = decode("guild.GuildPoolDonateResponse", body2).get("code") if st2 == 200 else st2
        if lv >= 2:
            break
    call("POST", "/admin/clock", b"reset=1", host="127.0.0.1")
    check("the Wishing Pool: no donation on the founding day, 4 stars a day, level 2 once all three colors reach 150",
          st0 == 400 and over == 3 and lv == 2 and all(c == 1 for c in codes), f"{st0}, {over}, lv {lv}, {len(codes)} days, {set(codes)}")

    st, body = call("GET", "/admin", host=GAME)
    check("admin page is not reachable through the game host", b"Accounts" not in body)
    st, _ = call("POST", "/admin/hcoin", f"account={acc_id}&qty=1".encode(), host="127.0.0.1",
                 extra={"Origin": "http://somewhere.example"})
    st2, _ = call("POST", "/admin/hcoin", f"account={acc_id}&qty=1".encode(), host="127.0.0.1:8721",
                  extra={"Origin": "http://127.0.0.1:8721"})
    check("an admin form sent from another site is refused, from the admin page itself it works", st == 403 and st2 == 200,
          f"{st}, {st2}")

    # the platform calls, answered by this server (no captures)
    cats = "category=9b0aceb48c&category=228b53e117"
    st, body = call("GET", f"/v1/domain-proxy?{cats}", host="proxy.c37e4.com")
    via_proxy = json.loads(body)["data"]["domain_proxy"] if st == 200 else []
    st2, body2 = call("GET", f"/v1/domain-proxy/?{cats}", host="10.0.2.2:8721")
    direct = json.loads(body2)["data"]["domain_proxy"] if st2 == 200 else []
    if os.environ.get("WOOFIA_SDK_KEY"):
        check("the domain list has the asked categories; a direct client gets another login host, the same QooApp one",
              [d["category_id"] for d in via_proxy] == ["9b0aceb48c", "228b53e117"] and len(direct) == 2
              and direct[0]["domain_path"] != via_proxy[0]["domain_path"] and direct[1]["domain_path"] == via_proxy[1]["domain_path"],
              f"{st} {via_proxy}, {st2} {direct}")
    else:
        check("without the SDK key the domain list is not answered", st == 404 and st2 == 404, f"{st}, {st2}")
    st, body = call("GET", "/v1/accounts/functions/games/84f38c1514", host=LOGIN)
    st2, body2 = call("GET", "/v1/gamedata/news/publish-time?game_id=4&language=1", host="news-service.playhorny.com")
    check("the SDK switches (guest login on) and the news time answer", st == 200 and json.loads(body)["data"]["guest_login_status"] == 1
          and st2 == 200 and json.loads(body2)["data"]["publish_time"] == 0, f"{st}, {st2}")
    st, _ = call("GET", "/v1/something/else", host=LOGIN)
    check("an unknown PlayHorny call is not answered", st == 404, f"status {st}")
    cfg_path = "/config/xxlwoofia-config-android-cela-IAR17066.json"
    st, body = call("GET", cfg_path, host="assets.woofia.playhorny.com")
    st2, body2 = call("GET", cfg_path, host="10.0.2.2:8721")
    cfg, cfg2 = (json.loads(body) if st == 200 else {}), (json.loads(body2) if st2 == 200 else {})
    check("the client config points its stream at this server; a direct client's game and proxy too",
          cfg.get("sse_server_url", "").startswith("http://127.0.0.1") and cfg.get("game_server_url", "").startswith("https://game.")
          and cfg2.get("game_server_url") == "http://10.0.2.2:8721/" and all(c["proxy_api"] == "http://10.0.2.2:8721"
                                                                             for c in cfg2.get("cAPIConfigs", [])),
          f"{cfg.get('sse_server_url')}, {cfg2.get('game_server_url')}")
    st, body = call("GET", "/config/catalog/Android/art_BD202610061503.hash", host="assets.woofia.playhorny.com")
    check("a catalog hash the mirror lacks is made from its catalog", st == 200 and len(body) == 32, f"status {st}, {body[:40]}")

    print(f"{fails} failed")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
