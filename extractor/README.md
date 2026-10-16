# Table extractor

Extracts the game data the server needs from your own copy of the XXL WOOFIA CDN files: the
client's data tables (raw and decoded to JSON) and the English item and buddy names. No game data is included
here.

## Use

```
pip install -r requirements.txt
cp .env.example .env
python extract.py
```

The CDN folder is the one the server uses (`config/catalog/Android/` and `bundle/Android/` inside it). It and the
client's bundle key come from `.env` (`WOOFIA_CDN`, `WOOFIA_BUNDLE_KEY`; `.env.example` has working values), the
environment, or `--cdn` and `--key`. The output goes where the server looks by default:

| Output | Server setting |
|---|---|
| `tables/raw/` | `WOOFIA_RAW_TABLES` |
| `tables/json/` | `WOOFIA_TABLES` |
| `tables/items.json` | `WOOFIA_ITEM_NAMES` |

Run it again after a client update.

## Files

- `extract.py` finds the newest data catalog, decrypts its main_data bundle, writes every table, decodes the ones
  `schema.json` describes and reads the names from the localization bundles.
- `schema.json` the field layout of each decoded table (`int`, `bool`, `double`, `float`, `long`, `string`, lists,
  fixed-length arrays and nested structs).
