"""Extract the game data the server needs from your own copy of the XXL WOOFIA CDN files.

Writes, under the output folder (default `tables`):
  raw/<table>.bytes    every data table of the client's main_data bundle, as the client reads them
  json/<table>.json    the tables schema.json describes, decoded: {"struct": name, "rows": [...]}
  items.json           item and buddy names in English, from the client's localization: {"<id>": "<name>"}
  art/avatar/<id>.png  the profile avatar icons (item ids), for the server's admin panel
  art/loading.jpg      the loading screen picture (the admin panel's sign-in page)
  art/buddies.json     buddy id -> its first avatar's item id: {"<buddy>": <item>}

The main_data bundle is encrypted by the client; its key is the client's bundle key (WOOFIA_BUNDLE_KEY, or
--key). The CDN folder is the one the server uses (WOOFIA_CDN): config/catalog/Android/ and bundle/Android/.

Usage: python extract.py [--cdn <CDN folder>] [--out tables] [--key <16 characters>] [--no-art]
(the CDN folder and key can also come from WOOFIA_CDN and WOOFIA_BUNDLE_KEY, or a .env file: see .env.example)
Needs: UnityPy, pycryptodome, Pillow (pip install -r requirements.txt)
"""
import argparse
import glob
import io
import json
import os
import re
import struct
import sys

import UnityPy
from Crypto.Cipher import AES
from Crypto.Util.Padding import unpad

UnityPy.config.FALLBACK_UNITY_VERSION = "2022.3.62f2"
HERE = os.path.dirname(os.path.abspath(__file__))
NAME_TABLES = ("道具名稱表", "角色名字表")  # item names, buddy names


def newest_catalog(cdn):
    cats = sorted(glob.glob(os.path.join(cdn, "config", "catalog", "Android", "data_BD*.json")))
    if not cats:
        sys.exit(f"no data catalog under {cdn}/config/catalog/Android")
    return open(cats[-1], encoding="utf-8").read(), cats[-1]


def newest_art_catalog(cdn):
    """The art catalog (art_BD*.json): the art bundles are listed there, not in the data catalog."""
    cats = sorted(glob.glob(os.path.join(cdn, "config", "catalog", "Android", "art_BD*.json")))
    return open(cats[-1], encoding="utf-8").read() if cats else ""


def art(cdn, out, names):
    """The avatar icons and the loading picture, for the admin panel. Returns (icons, loading picture saved)."""
    catalog = newest_art_catalog(cdn)
    folder = os.path.join(out, "art")
    os.makedirs(os.path.join(folder, "avatar"), exist_ok=True)
    icons, loading = 0, False
    for pattern, want in ((r"ui_single_avatar_assets_all_[0-9a-f]{32}\.bundle", "avatar"),
                          (r"home_background_assets_assets_all_[0-9a-f]{32}\.bundle", "loading")):
        for name in sorted(set(re.findall(pattern, catalog))):
            path = os.path.join(cdn, "bundle", "Android", name)
            if not os.path.exists(path):
                continue
            for obj in UnityPy.load(path).objects:
                if obj.type.name != "Texture2D":
                    continue
                tex = obj.read()
                m = re.fullmatch(r"IP(\d+)", tex.m_Name)
                if want == "avatar" and m:
                    tex.image.save(os.path.join(folder, "avatar", m.group(1) + ".png"))
                    icons += 1
                elif want == "loading" and tex.m_Name == "bgv71001_loading":
                    tex.image.convert("RGB").save(os.path.join(folder, "loading.jpg"), quality=86)
                    loading = True
    # a buddy's avatars are items named "<buddy> Avatar I" (a shard item shares the buddy's id and name)
    avatar_ids = {n: int(i) for i, n in names.items() if n.endswith(" Avatar I")}
    buddies = {}
    for i, n in names.items():
        base = n[:-len(" Shard")] if n.endswith(" Shard") else n
        if f"{base} Avatar I" in avatar_ids:
            buddies[i] = avatar_ids[f"{base} Avatar I"]
    json.dump(buddies, open(os.path.join(folder, "buddies.json"), "w", encoding="utf-8"), indent=1)
    return icons, loading


def decrypt(data, key):
    """The client's bundle wrapper: [AES-CBC ciphertext][IV][int32 IV length], PKCS7."""
    n = struct.unpack_from("<i", data, len(data) - 4)[0]
    iv = data[len(data) - 4 - n:len(data) - 4]
    return unpad(AES.new(key, AES.MODE_CBC, iv).decrypt(data[:len(data) - 4 - n]), 16)


def raw_tables(cdn, catalog, key):
    """{table name: bytes} from the catalog's main_data bundle."""
    names = sorted(set(re.findall(r"main_data_assets_all_[0-9a-f]+\.bundle", catalog)))
    if len(names) != 1:
        sys.exit(f"expected one main_data bundle in the catalog, found {names}")
    path = os.path.join(cdn, "bundle", "Android", names[0])
    data = open(path, "rb").read()
    if not data.startswith(b"UnityFS"):
        try:
            data = decrypt(data, key)
        except ValueError:
            sys.exit("main_data did not decrypt: check the bundle key")
    env = UnityPy.load(io.BytesIO(data))
    out = {}
    for obj in env.objects:
        if obj.type.name == "TextAsset":
            ta = obj.read()
            script = ta.m_Script
            out[ta.m_Name] = script.encode("utf-8", "surrogateescape") if isinstance(script, str) else bytes(script)
    return out


class Reader:
    def __init__(self, data):
        self.b, self.p = data, 0

    def take(self, fmt):
        v = struct.unpack_from(fmt, self.b, self.p)[0]
        self.p += struct.calcsize(fmt)
        return v

    def string(self):
        n, shift = 0, 0
        while True:
            c = self.b[self.p]
            self.p += 1
            n |= (c & 0x7F) << shift
            if c < 0x80:
                break
            shift += 7
        if self.p + n > len(self.b):
            raise ValueError("string past the end")
        s = self.b[self.p:self.p + n].decode("utf-8")
        self.p += n
        return s

    def value(self, t):
        if isinstance(t, dict) and "list" in t:
            n = self.take("<i")
            if not 0 <= n <= 100000:
                raise ValueError(f"bad count {n}")
            return [self.value(t["list"]) for _ in range(n)]
        if isinstance(t, dict) and "fixed" in t:
            # an array of a set length, stored with no count (trailing zeros dropped when "trim" is set)
            items = [self.value(t["of"]) for _ in range(t["fixed"])]
            while t.get("trim") and items and items[-1] == 0:
                items.pop()
            return items
        if isinstance(t, dict) and "struct" in t:
            return {name: self.value(ft) for name, ft in t["struct"]}
        if t == "int":
            return self.take("<i")
        if t == "bool":
            return bool(self.take("<B"))
        if t == "double":
            return self.take("<d")
        if t == "float":
            return self.take("<f")
        if t == "long":
            return self.take("<q")
        if t == "string":
            return self.string()
        raise ValueError(f"unknown type {t}")


def decode(data, fields):
    r = Reader(data)
    rows = [{name: r.value(ft) for name, ft in fields} for _ in range(r.take("<i"))]
    if r.p != len(data):
        raise ValueError(f"stopped at {r.p} of {len(data)} bytes")
    return rows


def names(cdn, catalog):
    """English item and buddy names from the localization string bundles the catalog names."""
    bundles = sorted(set(re.findall(r"localization-remote-string_assets_(?:_|locale-en_)[0-9a-f]+\.bundle", catalog)))
    keys, texts = {}, {}
    for b in bundles:
        path = os.path.join(cdn, "bundle", "Android", b)
        if not os.path.exists(path):
            continue
        for obj in UnityPy.load(path).objects:
            if obj.type.name != "MonoBehaviour":
                continue
            tree = obj.read_typetree()
            if "m_Entries" in tree and "m_TableCollectionName" in tree:
                keys[tree["m_TableCollectionName"]] = {e["m_Id"]: e["m_Key"] for e in tree["m_Entries"]}
            elif "m_TableData" in tree and "m_SharedData" in tree:
                name = re.sub(r"_en$", "", tree.get("m_TableCollectionName") or tree.get("m_Name", ""))
                texts[name] = {e["m_Id"]: e["m_Localized"] for e in tree["m_TableData"]}
    out = {}
    for table in NAME_TABLES:
        k = keys.get(table, {})
        for i, text in texts.get(table, {}).items():
            key = k.get(i, "")
            if key.isdigit() and text:
                out.setdefault(key, text.strip())
    return out


def load_dotenv(path=".env"):
    """Settings from a .env file (KEY=value lines, # comments, optional double quotes); the environment wins."""
    if not os.path.exists(path):
        return
    for line in open(path, encoding="utf-8"):
        line = line.strip()
        if line.startswith("#") or "=" not in line:
            continue
        key, value = (x.strip() for x in line.split("=", 1))
        if len(value) >= 2 and value[0] == value[-1] == '"':
            value = value[1:-1]
        if key and value and not os.environ.get(key):
            os.environ[key] = value


def main():
    load_dotenv()
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--cdn", default=os.environ.get("WOOFIA_CDN"), help="your copy of the game's CDN files")
    ap.add_argument("--out", default="tables")
    ap.add_argument("--key", default=os.environ.get("WOOFIA_BUNDLE_KEY", ""), help="the client's bundle key")
    ap.add_argument("--no-art", action="store_true", help="skip the admin panel's avatars and picture")
    a = ap.parse_args()
    if not a.cdn:
        sys.exit("give the CDN folder (--cdn or WOOFIA_CDN)")
    if len(a.key) != 16:
        sys.exit("the bundle key (--key or WOOFIA_BUNDLE_KEY) must be 16 characters")
    catalog, catalog_path = newest_catalog(a.cdn)
    print(f"catalog {os.path.basename(catalog_path)}")
    tables = raw_tables(a.cdn, catalog, a.key.encode())
    os.makedirs(os.path.join(a.out, "raw"), exist_ok=True)
    os.makedirs(os.path.join(a.out, "json"), exist_ok=True)
    for name, data in tables.items():
        open(os.path.join(a.out, "raw", name + ".bytes"), "wb").write(data)
    print(f"{len(tables)} raw tables")
    schema = json.load(open(os.path.join(HERE, "schema.json"), encoding="utf-8"))
    ok, failed = 0, []
    for table, spec in sorted(schema.items()):
        if table not in tables:
            failed.append(f"{table} (not in main_data)")
            continue
        try:
            rows = decode(tables[table], spec["fields"])
        except (ValueError, struct.error, IndexError, UnicodeDecodeError) as e:
            failed.append(f"{table} ({e})")
            continue
        with open(os.path.join(a.out, "json", table + ".json"), "w", encoding="utf-8") as f:
            json.dump({"struct": spec["struct"], "rows": rows}, f, ensure_ascii=False, indent=1)
        ok += 1
    print(f"{ok} tables decoded" + (f"; failed: {', '.join(failed)}" if failed else ""))
    n = names(a.cdn, catalog)
    json.dump(n, open(os.path.join(a.out, "items.json"), "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    print(f"{len(n)} item and buddy names")
    if not a.no_art:
        icons, loading = art(a.cdn, a.out, n)
        print(f"{icons} avatar icons" + ("" if loading else "; no loading picture found"))
    if failed:
        sys.exit(1)


if __name__ == "__main__":
    main()
