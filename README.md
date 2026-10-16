# XXLHunks

A research server emulator for the mobile game **XXL WOOFIA**, written so the game can be studied, preserved and
played after the official servers close. Every call the client makes is answered by the server's own logic.

This project is not affiliated with or endorsed by the game's developers or PlayHorny. It contains no game files:
the data it needs comes from your own copy of the game.

## What works

- Logging in through a stand-in for the PlayHorny account service (logins are made on the admin page), link codes
  to move an account between devices, and new accounts that start like the game's own.
- Story, event and challenge stages, towers, sweeps and stage rewards.
- Summons (banners, pity, wishlists, step rewards, the first-time buddy pick), buddies (levels, limit breaks,
  ranks, skills, sigils, outfits) and hook-up rooms with bond levels.
- Missions, sign-ins, mail, the bag, stores, the mall (packs, monthly cards, battle passes) and part-time jobs.
- Past events rerun from the admin page, with their minigames: bingo, poking boards, the wheel, BBQ, finding
  characters, votes and Sausage Bro. Some older events need art that the current game files no longer have.
- Guilds (wishes, the wishing pool, the board, roles) and the Escape Room with live updates.
- Friends: follows, feeds, pokes, likes and the weekly ranking.
- An admin panel for running the server (see below).

Not done: real-money purchases (H Coin is set on the admin page), and features the client has no screens for.

## What you need

- Rust 1.85 or later, and Python 3 for the table extractor and the tests.
- Your own copy of the game's CDN files: configs, catalogs, bundles and hot-update files (`config/`, `bundle/`,
  `hotupdate/`, `version/`).
- The game's data tables, which the extractor in `extractor/` writes from those CDN files.

## Setting it up

```
cp .env.example .env
(cd extractor && pip install -r requirements.txt && cp .env.example .env && python extract.py --out ../tables)
cargo build --release
./target/release/woofia-server
```

Point `WOOFIA_CDN` in both `.env` files at your CDN copy first. Then open http://127.0.0.1:8720/admin on the same
machine and add a login (name and password) for the game's login screen. An account is made the first time a
login reaches the game.

## Settings

Settings come from the environment or a `.env` file in the folder the server is started in. `.env.example` has
every one with working values; the environment wins over `.env`.

| Variable | Default | |
|---|---|---|
| `WOOFIA_ADDR` | `127.0.0.1:8720` | address to listen on |
| `WOOFIA_DATA` | `data` | saved accounts, logins, guilds, logs |
| `WOOFIA_CDN` | `cdn` | your copy of the CDN files |
| `WOOFIA_TABLES` | `tables/json` | JSON tables (from the extractor) |
| `WOOFIA_RAW_TABLES` | `tables/raw` | raw table files (from the extractor) |
| `WOOFIA_ITEM_NAMES` | `tables/items.json` | item and buddy names for the admin panel (from the extractor) |
| `WOOFIA_ART` | `tables/art` | avatars and the sign-in picture for the admin panel (from the extractor) |
| `WOOFIA_BUNDLE_KEY` | | the client's bundle key: lets the server write event dates into the game's tables |
| `WOOFIA_SDK_KEY`, `WOOFIA_SDK_NONCE` | | the login kit's key and nonce: lets the server answer its domain lists |
| `WOOFIA_SSE_URL` | `http://127.0.0.1:8720/` | live update address given to clients that come through the proxy |
| `WOOFIA_ADMIN_KEY` | | lets other machines sign in to the admin panel; 16 characters or more |
| `WOOFIA_REPLAY` | | a folder of captured replies, for calls the server has no logic for (research only) |
| `WOOFIA_TEST_CLOCK` | | tests only: the admin panel can move the game clock |

## The admin panel

http://127.0.0.1:8720/admin, in the game's look:
- a dashboard: who is online and where, running events, recent changes;
- accounts: search, suspend or ban, rename, logins and devices, delete with a backup first;
- each player's buddies, items, mailbox, stages and guild;
- an event calendar: rerun past events, change dates;
- guilds: roles, kick, rename, disband, the Escape Room;
- redeem codes with end dates and use limits, and mail to many players now or at a set time;
- the live log and a record of every change (`data/logs/admin.log`);
- backups: snapshots of everything or one account, restore, a daily automatic snapshot, download as `.tar`;
- the settings and test tools.

It answers the machine the server runs on without a key. Any other machine, including a browser outside a Docker
container, signs in with `WOOFIA_ADMIN_KEY`; with no key set, other machines get nothing. The avatars and the
sign-in picture come from the extractor's art step; without them the panel shows initials.

## Connecting the game

The game talks to its official hosts over HTTPS. `redirect.py` is a mitmproxy addon that sends those hosts to
this server and keeps their `Host` header:

```
mitmdump --listen-host 127.0.0.1 -p 8089 --set connection_strategy=lazy --set upstream_cert=false -s redirect.py
```

The device needs the mitmproxy CA in its system certificate store and its proxy set to the mitmdump port. On an
Android emulator, `adb reverse tcp:8089 tcp:8089` and `adb reverse tcp:8720 tcp:8720` let it reach the PC. The
server also answers a client whose built-in addresses point at it directly, telling the official host each call
stands for from its path.

## Layout

- `src/` the server. `game.rs` routes the game's calls; the feature modules (`stages.rs`, `chars.rs`, `mall.rs`,
  `guilds.rs`, `dungeon.rs`, `events.rs`, `missions.rs`, `social.rs` and others) hold their logic.
  `playhorny.rs` is the login service stand-in, `direct.rs` the platform calls.
- `admin/` the admin panel's page, styles and script (built into the server); `src/adminweb.rs` its sign-in and
  actions, `src/adminapi.rs` its read API, `src/manage.rs` bans, codes and mail to many players,
  `src/backups.rs` snapshots.
- `src/fresh_account.json` a new account's starting state.
- `proto/` the game's message definitions; `build.rs` turns them into Rust types.
- `extractor/` the table extractor (Python).
- `redirect.py` the mitmproxy addon.
- `tests/smoke.py` end-to-end checks; `tests/run_smoke.sh` runs them against a test server on an empty data folder.

## License

MIT, see `LICENSE`.
