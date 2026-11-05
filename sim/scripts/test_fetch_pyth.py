"""python3 scripts/test_fetch_pyth.py"""
import pathlib
import sys
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import fetch_pyth  # noqa: E402


class FakeCache:
    def __init__(self, seconds: dict):
        self.seconds = seconds
        self.asked = []

    def get(self, key: str, t: int) -> dict:
        self.asked.append(t)
        return self.seconds.get(t, {})


def price(publish_time: int) -> dict:
    return {"publish_time": publish_time}


class Sharpest(unittest.TestCase):
    def series(self, jumps: dict) -> dict:
        """10,000 flat seconds on every market, with `jumps` {second: relative move} on SOL"""
        flat = {t: 100.0 for t in range(10_000)}
        sol = dict(flat)
        level = 100.0
        for t in range(10_000):
            level *= 1 + jumps.get(t, 0.0)
            sol[t] = level
        return {"SOL": sol, "BTC": flat, "ETH": flat}

    def pick(self, jumps: dict) -> list:
        series = self.series(jumps)
        with mock.patch.object(fetch_pyth, "closes", lambda a, day: series[a]):
            return fetch_pyth.sharpest("2026-01-01")

    def test_takes_the_largest_moves_apart(self):
        picked = self.pick({1_000: 0.02, 5_000: -0.03})
        self.assertEqual(len(picked), fetch_pyth.PER_DAY)
        self.assertEqual(picked, sorted(picked))
        self.assertTrue(any(1_001 <= t <= 1_004 for t in picked))
        self.assertTrue(any(5_001 <= t <= 5_004 for t in picked))
        self.assertTrue(all(b - a >= fetch_pyth.SPACING_SEC for a, b in zip(picked, picked[1:])))

    def test_any_market_counts(self):
        series = self.series({})
        series["ETH"] = {t: 100.0 if t < 7_000 else 110.0 for t in range(10_000)}
        with mock.patch.object(fetch_pyth, "closes", lambda a, day: series[a]):
            self.assertTrue(any(7_001 <= t <= 7_003 for t in fetch_pyth.sharpest("2026-01-01")))


class FirstFrom(unittest.TestCase):
    def test_takes_the_first_second_with_an_update_for_each_feed(self):
        cache = FakeCache({
            10: {"SOL": price(10)},
            11: {"SOL": price(11), "BTC": price(11)},
            12: {"BTC": price(12), "ETH": price(12)},
            13: {"SOL": price(13), "BTC": price(13), "ETH": price(13)},
        })
        out = fetch_pyth.first_from(cache, "key", 10)
        self.assertEqual({a: p["publish_time"] for a, p in out.items()}, {"SOL": 10, "BTC": 11, "ETH": 12})
        self.assertEqual(cache.asked, [10, 11, 12])  # stops once every feed has one

    def test_gives_up_after_ten_seconds(self):
        cache = FakeCache({10: {"SOL": price(10)}, 20: {"BTC": price(20)}})
        self.assertEqual(set(fetch_pyth.first_from(cache, "key", 10)), {"SOL"})
        self.assertEqual(cache.asked, list(range(10, 20)))


if __name__ == "__main__":
    unittest.main()
