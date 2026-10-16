import unittest

from invoice.calc import calc


ITEMS = [{"name": "pen", "price": 2.5, "qty": 4}, {"name": "pad", "price": 5.0, "qty": 1}]


class CalcTest(unittest.TestCase):
    def test_no_tax(self):
        self.assertEqual(calc(ITEMS, 0), 15.0)

    def test_tax_is_a_percentage(self):
        self.assertEqual(calc(ITEMS, 10), 16.5)

    def test_discount_before_tax(self):
        self.assertEqual(calc(ITEMS, 10, discount=0.2), 13.2)


if __name__ == "__main__":
    unittest.main()
