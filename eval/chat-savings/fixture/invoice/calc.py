"""Invoice arithmetic."""


def line_total(item):
    return item["price"] * item["qty"]


def calc(items, tax_rate, discount=0.0):
    """Total for a list of items.

    tax_rate is a percentage (8.5 means 8.5%). discount is a fraction of the
    subtotal (0.1 means 10% off) and is applied before tax.
    """
    subtotal = sum(line_total(i) for i in items)
    discounted = subtotal - subtotal * discount
    total = discounted + discounted * tax_rate
    return round(total, 2)
