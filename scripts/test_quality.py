"""Regression tests for Cargo audit policy decisions."""

import contextlib
import io
import unittest

from check_audit import check_report


class AuditPolicyTests(unittest.TestCase):
    def setUp(self):
        self.enterContext(contextlib.redirect_stderr(io.StringIO()))
        self.report = {"warnings": {}, "vulnerabilities": {"found": False, "list": []}}

    def test_only_reviewed_version_and_category_are_accepted(self):
        warning = {
            "advisory": {"id": "RUSTSEC-2026-0253"},
            "package": {"name": "lru", "version": "0.16.4"},
        }
        self.report["warnings"] = {"unsound": [warning]}
        self.assertEqual(check_report(self.report), 0)
        self.report["warnings"] = {"yanked": [warning]}
        self.assertEqual(check_report(self.report), 1)
        self.report["warnings"] = {"unsound": [warning]}
        warning["package"]["version"] = "0.16.5"
        self.assertEqual(check_report(self.report), 1)

    def test_vulnerabilities_and_yanks_without_advisory_ids_fail(self):
        self.report["vulnerabilities"]["found"] = True
        self.assertEqual(check_report(self.report), 1)
        self.report["vulnerabilities"]["found"] = False
        self.report["warnings"] = {
            "yanked": [{"package": {"name": "x", "version": "1"}}]
        }
        self.assertEqual(check_report(self.report), 1)
