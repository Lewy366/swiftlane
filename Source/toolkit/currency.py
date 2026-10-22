"""Currency converter using the European Central Bank's daily reference rates
(via the free Frankfurter API, no account). The last rates are kept on disk so it
still works offline; they're refreshed at most every few hours.
"""
import json
import time
import urllib.request
from pathlib import Path

API = "https://api.frankfurter.dev/v1"
REFRESH_SECONDS = 6 * 3600
NAMES = {  # used if the names can't be downloaded
    "EUR": "Euro", "USD": "US Dollar", "GBP": "British Pound", "CHF": "Swiss Franc",
    "JPY": "Japanese Yen", "CNY": "Chinese Yuan", "CAD": "Canadian Dollar", "AUD": "Australian Dollar",
}


class Rates:
    def __init__(self, cache_file):
        self.cache_file = Path(cache_file)
        self.data = self._load()

    def _load(self):
        try:
            return json.loads(self.cache_file.read_text("utf-8"))
        except (OSError, ValueError):
            return {}

    def _fetch(self, path):
        req = urllib.request.Request(f"{API}/{path}", headers={"User-Agent": "Swiftlane"})
        with urllib.request.urlopen(req, timeout=10) as resp:
            return json.loads(resp.read().decode("utf-8"))

    def refresh(self, force=False):
        """Get fresh rates if the saved ones are old. Returns status() either way."""
        fresh = time.time() - self.data.get("fetched", 0) < REFRESH_SECONDS
        if fresh and not force and self.data.get("rates"):
            return self.status()
        try:
            latest = self._fetch("latest?base=EUR")
            try:
                names = self._fetch("currencies")
            except Exception:
                names = self.data.get("names") or NAMES
            rates = {"EUR": 1.0, **{k: float(v) for k, v in latest["rates"].items()}}
            self.data = {"rates": rates, "date": latest.get("date"), "names": names, "fetched": time.time()}
            self.cache_file.parent.mkdir(parents=True, exist_ok=True)
            self.cache_file.write_text(json.dumps(self.data), "utf-8")
            self.data["error"] = None
        except Exception as e:
            self.data["error"] = f"Couldn't update rates ({e.__class__.__name__}); using saved ones." \
                if self.data.get("rates") else "No internet connection and no saved rates yet."
        return self.status()

    def status(self):
        rates = self.data.get("rates") or {}
        names = self.data.get("names") or NAMES
        return {"date": self.data.get("date"), "error": self.data.get("error"),
                "currencies": [{"code": c, "name": names.get(c, c)} for c in sorted(rates)]}

    def convert(self, amount, src, dst):
        rates = self.data.get("rates") or {}
        if src not in rates or dst not in rates:
            raise ValueError("Unknown currency")
        value = float(amount) / rates[src] * rates[dst]
        return {"value": value, "rate": rates[dst] / rates[src], "date": self.data.get("date")}
