"""Samples Pyth prices from Hermes around chosen moments, for the latency study (src/latency.rs).

  python3 scripts/fetch_pyth.py --key-file PATH [--top] DAY ...    # PATH holds PYTH_API_KEY=...

For each day, 100 moments t: 50 at random and 50 where some market moved more than 3 bps on Binance in the
three seconds before. For each, Hermes' update for the second t + offset for every offset in OFFSETS, which for
a past second is the first update published in it (prev_publish_time < second). Results go to
data/pyth/samples-DAY.json; Binance 1s klines for the day must be downloaded first.

With --top, the 100 moments are the day's sharpest instead, where the rare wide gaps are: the largest move on
Binance over those three seconds, at least SPACING_SEC apart. They go to data/pyth/samples-DAY-top.json.

Hermes rate-limits hard, so requests run one at a time, about one a second, and every answer is kept in
data/pyth/cache-DAY.jsonl: a run that stops picks up where it left off. The cache is not part of the data set.
"""
import csv
import datetime
import json
import pathlib
import random
import re
import sys
import time
import urllib.error
import urllib.request

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from fetch_binance import ROOT, manifest  # noqa: E402

FEEDS = {
    "SOL": "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d",
    "BTC": "e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43",
    "ETH": "ff61491a931112ddf1bd8147cd1b641375f79f5825126d665480874634fd0ace",
}
# what the user sees at t, the open fill at t + 2, and close fills for closes requested 0, 3 and 8 s later
OFFSETS = [0, 2, 4, 7, 12]
PER_DAY = 100
PACE_SEC = 1.0
FAST_BPS = 3.0
MAX_EMPTY = 5
SPACING_SEC = 15


def closes(asset: str, day: str) -> dict:
    out = {}
    for row in csv.reader(open(ROOT / "binance" / "1s" / f"{asset}USDT-1s-{day}.csv")):
        t = int(row[0])
        t = t // 1000 if t > 10**14 else t
        out[t // 1000] = float(row[4])
    return out


def moments(day: str, rng: random.Random) -> list:
    series = {a: closes(a, day) for a in FEEDS}
    start = int(datetime.datetime.fromisoformat(day).replace(tzinfo=datetime.timezone.utc).timestamp())
    seconds = [t for t in range(start + 10, start + 86_400 - 20) if all(t - 4 in s and t - 1 in s for s in series.values())]
    fast = [t for t in seconds if any(abs(s[t - 1] / s[t - 4] - 1) * 1e4 > FAST_BPS for s in series.values())]
    picked = set(rng.sample(seconds, PER_DAY // 2)) | set(rng.sample(fast, min(len(fast), PER_DAY // 2)))
    return sorted(picked)


def sharpest(day: str) -> list:
    series = {a: closes(a, day) for a in FEEDS}
    seconds = [t for t in next(iter(series.values())) if all(t - 4 in s and t - 1 in s for s in series.values())]
    move = lambda t: max(abs(s[t - 1] / s[t - 4] - 1) for s in series.values())
    picked: list = []
    for t in sorted(seconds, key=move, reverse=True):
        if all(abs(t - p) >= SPACING_SEC for p in picked):
            picked.append(t)
            if len(picked) == PER_DAY:
                break
    return sorted(picked)


def second(key: str, t: int) -> dict:
    """Hermes' first update in second t for each feed that has one (404: none has)"""
    ids = "&".join(f"ids[]={f}" for f in FEEDS.values())
    req = urllib.request.Request(
        f"https://hermes.pyth.network/v2/updates/price/{t}?{ids}&parsed=true&encoding=hex",
        headers={"Authorization": f"Bearer {key}", "User-Agent": "bust-sim/0.1"},
    )
    for attempt in range(12):
        try:
            with urllib.request.urlopen(req, timeout=30) as r:
                parsed = json.load(r)["parsed"]
            by_id = {p["id"]: p for p in parsed}
            return {
                a: {
                    "price": int(by_id[f]["price"]["price"]),
                    "conf": int(by_id[f]["price"]["conf"]),
                    "expo": by_id[f]["price"]["expo"],
                    "publish_time": by_id[f]["price"]["publish_time"],
                    "prev_publish_time": by_id[f]["metadata"]["prev_publish_time"],
                }
                for a, f in FEEDS.items()
                if f in by_id
            }
        except urllib.error.HTTPError as e:
            if e.code == 404:
                return {}
            print(f"  HTTP {e.code} at {t}, waiting", flush=True)
            time.sleep(60)  # rate limited
        except Exception as e:
            print(f"  {e} at {t}, waiting", flush=True)
            time.sleep(30)
    raise RuntimeError(f"Hermes failed for {t}")


class Cache:
    """Answers per second, appended to a file as they arrive"""

    def __init__(self, day: str):
        self.path = ROOT / "pyth" / f"cache-{day}.jsonl"
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.seen = {}
        if self.path.exists():
            for line in self.path.read_text().splitlines():
                t, value = json.loads(line)
                self.seen[t] = value

    def get(self, key: str, t: int) -> dict:
        if t not in self.seen:
            time.sleep(PACE_SEC)
            self.seen[t] = second(key, t)
            with self.path.open("a") as f:
                f.write(json.dumps([t, self.seen[t]]) + "\n")
        return self.seen[t]


def first_from(cache: Cache, key: str, t: int) -> dict:
    """The first update at or after second t for each feed, as the program takes a fill price: if a second has none,
    the next second that does. Feeds with nothing within 10 s are left out"""
    out: dict = {}
    for s in range(t, t + 10):
        for a, px in cache.get(key, s).items():
            out.setdefault(a, px)
        if len(out) == len(FEEDS):
            break
    return out


def main() -> None:
    if len(sys.argv) < 4 or sys.argv[1] != "--key-file":
        raise SystemExit(__doc__)
    key = re.search(r'^PYTH_API_KEY\s*=\s*"?([^"\n]+)"?', pathlib.Path(sys.argv[2]).read_text(), re.M).group(1)
    top = "--top" in sys.argv
    for day in [a for a in sys.argv[3:] if a != "--top"]:
        out = ROOT / "pyth" / f"samples-{day}{'-top' if top else ''}.json"
        if out.exists():
            continue
        rng = random.Random(day)
        ts = sharpest(day) if top else moments(day, rng)
        cache = Cache(day)
        samples = []
        empty = 0
        for n, t in enumerate(ts):
            sample = {str(o): first_from(cache, key, t + o) for o in OFFSETS}
            samples.append({"t": t, "pyth": sample})
            empty = 0 if any(sample.values()) else empty + 1
            if empty == MAX_EMPTY:
                raise SystemExit(f"{day}: {empty} moments in a row with no prices, Hermes' archive has a hole here")
            print(f"{day}: {n + 1}/{len(ts)} moments, {len(cache.seen)} seconds fetched", flush=True)
        out.write_text(json.dumps(samples))
    manifest()


if __name__ == "__main__":
    main()
