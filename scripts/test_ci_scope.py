import tempfile
import unittest
from pathlib import Path

from ci_scope import BROWSERS, select


class ScopeTests(unittest.TestCase):
    def test_docs_retain_only_the_main_checks(self):
        plan = select(["README.md"])
        self.assertFalse(plan["windows_dependencies"])
        self.assertFalse(plan["native_bindings"])
        self.assertFalse(plan["consumers"])
        self.assertEqual(plan["browser"], [])

    def test_shared_dependency_changes_require_all_checks(self):
        plan = select(["Cargo.lock"])
        self.assertTrue(plan["windows_dependencies"])
        self.assertTrue(plan["native_bindings"])
        self.assertTrue(plan["consumers"])
        self.assertEqual(plan["browser"], list(BROWSERS))

    def test_dependency_script_changes_keep_windows_validation(self):
        self.assertTrue(select(["scripts/release_dependencies.py"])["windows_dependencies"])

    def test_native_ffi_changes_select_native_checks(self):
        plan = select(["crates/app-core-bindings/src/ffi.rs"])
        self.assertTrue(plan["native_bindings"])
        self.assertFalse(plan["consumers"])

    def test_core_payload_changes_check_native_types_and_consumer(self):
        plan = select(["crates/app-core/src/history/model.rs"])
        self.assertTrue(plan["consumers"])
        self.assertTrue(plan["native_bindings"])

    def test_shared_web_components_run_all_browser_suites(self):
        self.assertEqual(select(["crates/web-ui/src/lib.rs"])["browser"], list(BROWSERS))

    def test_shared_geometry_runs_browser_suites(self):
        self.assertEqual(select(["crates/history-geometry/src/lib.rs"])["browser"], list(BROWSERS))

    def test_fixture_change_selects_one_browser_suite(self):
        self.assertEqual(select(["crates/web-ui/examples/sessions.rs"])["browser"], ["sessions"])

    def test_css_selects_every_fixture_that_copies_it(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            for suite in ("history", "navigation"):
                (root / f"scripts/build-{suite}.sh").write_text("cp crates/web-ui/assets/history.css out/\n")
            plan = select(["crates/web-ui/assets/history.css"], root=root)
            self.assertEqual(plan["browser"], ["history", "navigation"])

    def test_release_and_integration_runs_are_full(self):
        self.assertEqual(select([], full=True)["browser"], list(BROWSERS))


if __name__ == "__main__":
    unittest.main()
