"""Regression checks for the nonvacuous mutation policy."""

import unittest

from check_mutation_outcomes import verify_mutants


class MutationOutcomesTests(unittest.TestCase):
    @staticmethod
    def counts(**changes):
        return {
            "total_mutants": 0,
            "caught": 0,
            "unviable": 0,
            "missed": 0,
            "timeout": 0,
            "success": 0,
        } | changes

    def test_refuses_empty_or_all_unviable_results(self):
        for data in (self.counts(), self.counts(total_mutants=2, unviable=2)):
            with (
                self.subTest(data=data),
                self.assertRaisesRegex(ValueError, "nonempty"),
            ):
                verify_mutants(data)

    def test_refuses_any_surviving_timed_out_or_unresolved_case(self):
        for category in ("missed", "timeout", "success"):
            data = self.counts(total_mutants=3, caught=2, **{category: 1})
            with (
                self.subTest(category=category),
                self.assertRaisesRegex(ValueError, "expected"),
            ):
                verify_mutants(data)

    def test_accepts_a_reconciled_viable_corpus(self):
        self.assertEqual(
            verify_mutants(self.counts(total_mutants=5, caught=4, unviable=1)), (4, 1)
        )

    def test_refuses_a_report_with_an_inconsistent_total(self):
        with self.assertRaisesRegex(ValueError, "reconcile"):
            verify_mutants(self.counts(total_mutants=2, caught=1))
