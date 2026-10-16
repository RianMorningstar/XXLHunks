"""Extract the game data the server needs from your own copy of the XXL WOOFIA CDN files.

Writes, under the output folder (default `tables`):
  raw/<table>.bytes    every data table of the client's main_data bundle, as the client reads them
  json/<table>.json    the tables schema.json describes, decoded: {"struct": name, "rows": [...]}
  items.json           item and buddy names in English, from the client's localization: {"<id>": "<name>"}

The main_data bundle is encrypted by the client; its key is the client's bundle key (WOOFIA_BUNDLE_KEY, or
--key). The CDN folder is the one the server uses (WOOFIA_CDN): config/catalog/Android/ and bundle/Android/.

Usage: python extract.py [--cdn <CDN folder>] [--out tables] [--key <16 characters>]
(the CDN folder and key can also come from WOOFIA_CDN and WOOFIA_BUNDLE_KEY, or a .env file: see .env.example)
Needs: UnityPy, pycryptodome (pip install -r requirements.txt)
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
    if failed:
        sys.exit(1)


if __name__ == "__main__":
    main()
