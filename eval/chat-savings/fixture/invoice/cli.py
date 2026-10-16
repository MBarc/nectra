"""Command line: python -m invoice.cli ITEMS.json --tax 8.5 [--discount 0.1]"""
import argparse
import json

from invoice.calc import calc


def main(argv=None):
    parser = argparse.ArgumentParser(description="Total an invoice.")
    parser.add_argument("items", help="JSON file: a list of {name, price, qty}")
    parser.add_argument("--tax", type=float, default=0.0, help="tax rate in percent")
    parser.add_argument("--discount", type=float, default=0.0, help="fraction off, e.g. 0.1")
    args = parser.parse_args(argv)
    with open(args.items, encoding="utf-8") as f:
        items = json.load(f)
    print(f"Total: {calc(items, args.tax, args.discount):.2f}")


if __name__ == "__main__":
    main()
