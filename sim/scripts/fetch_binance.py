"""Downloads Binance spot klines from data.binance.vision into sim/data/binance and records their SHA-256.

  python3 sim/scripts/fetch_binance.py minute           # 1m: the last 12 months and the crisis months
  python3 sim/scripts/fetch_binance.py second DAY ...   # 1s for the given days (YYYY-MM-DD)

Archives are kept as downloaded; sim/data/MANIFEST.sha256 lists every file so a run can be checked against
the data it was made with.
"""
import hashlib
import io
import pathlib
import sys
import urllib.error
import urllib.request
import zipfile

ROOT = pathlib.Path(__file__).resolve().parent.parent / "data"
SYMBOLS = ["BTCUSDT", "ETHUSDT", "SOLUSDT"]
LAST_12 = [f"{y}-{m:02d}" for y, m in [(2025, 11), (2025, 12)] + [(2026, m) for m in range(1, 11)]]
CRISES = ["2020-03", "2021-05", "2022-05", "2022-11", "2024-08", "2025-10"]


def fetch(url: str, dest: pathlib.Path) -> bool:
    if dest.exists():
        return True
    try:
        with urllib.request.urlopen(url, timeout=120) as r:
            data = r.read()
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return False
        raise
    with zipfile.ZipFile(io.BytesIO(data)) as z:
        (name,) = z.namelist()
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_bytes(z.read(name))
    return True


def manifest() -> None:
    lines = []
    for f in sorted((ROOT / "binance").rglob("*.csv")):
        lines.append(f"{hashlib.sha256(f.read_bytes()).hexdigest()}  {f.relative_to(ROOT)}")
    (ROOT / "MANIFEST.sha256").write_text("\n".join(lines) + "\n")


def main() -> None:
    kind = sys.argv[1]
    if kind == "minute":
        for s in SYMBOLS:
            for month in LAST_12 + CRISES:
                url = f"https://data.binance.vision/data/spot/monthly/klines/{s}/1m/{s}-1m-{month}.zip"
                ok = fetch(url, ROOT / "binance" / "1m" / f"{s}-1m-{month}.csv")
                print(f"{s} {month}: {'ok' if ok else 'not listed'}")
    elif kind == "second":
        for s in SYMBOLS:
            for day in sys.argv[2:]:
                url = f"https://data.binance.vision/data/spot/daily/klines/{s}/1s/{s}-1s-{day}.zip"
                ok = fetch(url, ROOT / "binance" / "1s" / f"{s}-1s-{day}.csv")
                print(f"{s} {day}: {'ok' if ok else 'not listed'}")
    else:
        raise SystemExit(__doc__)
    manifest()


if __name__ == "__main__":
    main()
