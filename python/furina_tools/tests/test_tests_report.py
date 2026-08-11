import unittest

from furina_tools.tests_report import detect_framework, parse_test_report


class TestTestsReport(unittest.TestCase):
    def test_detect_framework(self):
        self.assertEqual(detect_framework("python -m pytest -q"), "pytest")
        self.assertEqual(detect_framework("cargo test"), "cargo")
        self.assertEqual(detect_framework("npm test"), "npm")
        self.assertEqual(detect_framework("mvn test"), "maven")
        self.assertEqual(detect_framework("go test ./..."), "go")

    def test_assertion_failure(self):
        out = (
            "============================= FAILURES =============================\n"
            "___________ CalculatorTest.test_add ___________\n"
            "    def test_add(self):\n"
            ">       self.assertEqual(add(2, 3), 5)\n"
            "E       AssertionError: -1 != 5\n"
            "1 failed, 1 passed in 0.15s\n"
        )
        r = parse_test_report("python -m pytest", out, "", 1)
        self.assertFalse(r["passed"])
        self.assertEqual(r["failed_count"], 1)
        self.assertEqual(r["passed_count"], 1)
        self.assertTrue(any(e["category"] == "assertion" for e in r["errors"]))

    def test_compile_error(self):
        out = "error[E0425]: cannot find value `foo` in this scope\n --> src/main.rs:3:9"
        r = parse_test_report("cargo test", out, "", 1)
        self.assertTrue(any(e["category"] == "compile" for e in r["errors"]))

    def test_dependency_error(self):
        out = "ModuleNotFoundError: No module named 'requests'"
        r = parse_test_report("python -m pytest", out, "", 1)
        self.assertTrue(any(e["category"] == "dependency" for e in r["errors"]))

    def test_timeout(self):
        out = "Test timed out after 30s"
        r = parse_test_report("go test", out, "", 1)
        self.assertTrue(any(e["category"] == "timeout" for e in r["errors"]))

    def test_success(self):
        out = "3 passed in 0.01s"
        r = parse_test_report("python -m pytest", out, "", 0)
        self.assertTrue(r["passed"])
        self.assertEqual(r["passed_count"], 3)


if __name__ == "__main__":
    unittest.main()
