"""Check which dependency updates are eligible for unattended adoption."""

from copy import deepcopy
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("merge_release_update", Path(__file__).with_name("merge-release-update.py"))
MERGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MERGE)


class DependencyAdoptionTests(unittest.TestCase):
    def setUp(self):
        self.repository = "idleai/fixture"
        self.run = {"conclusion": "success", "event": "workflow_dispatch", "head_branch": MERGE.BRANCH,
                    "head_repository": {"full_name": self.repository}, "head_sha": "a" * 40}
        self.pull = {"state": "open", "draft": False, "user": {"login": "github-actions[bot]"},
                     "head": {"repo": {"full_name": self.repository}, "ref": MERGE.BRANCH, "sha": "a" * 40},
                     "base": {"ref": "main"}}

    def test_only_current_successful_bot_commit_can_merge(self):
        self.assertTrue(MERGE.eligible(self.run, self.pull, self.repository))
        for field, value in (("conclusion", "failure"), ("event", "pull_request"), ("head_sha", "b" * 40),
                             ("head_branch", "feature"), ("head_repository", {"full_name": "fork/fixture"})):
            run = dict(self.run, **{field: value})
            self.assertFalse(MERGE.eligible(run, self.pull, self.repository), field)
        for field, value in (("state", "closed"), ("draft", True), ("user", {"login": "contributor"})):
            self.assertFalse(MERGE.eligible(self.run, dict(self.pull, **{field: value}), self.repository), field)

    def test_compatibility_boundaries_include_zero_versions(self):
        compatible = MERGE.updater().compatible
        self.assertTrue(compatible((1, 2, 3), (1, 9, 0)))
        self.assertFalse(compatible((1, 2, 3), (2, 0, 0)))
        self.assertTrue(compatible((0, 2, 3), (0, 2, 4)))
        self.assertFalse(compatible((0, 2, 3), (0, 3, 0)))
        self.assertTrue(compatible((0, 0, 3), (0, 0, 3)))
        self.assertFalse(compatible((0, 0, 3), (0, 0, 4)))

    @staticmethod
    def artifact(version="0.1.0", checksum="a" * 64):
        return {"schema": 1, "dependencies": {"host": {
            "repository": "idleai/host-tools", "tag": f"host-v{version}", "sha256": {"linux-x64": checksum}}}}

    def test_artifact_upgrade_must_be_compatible_and_match_published_checksums(self):
        resolver = MERGE.updater()
        before, after = self.artifact(), self.artifact("0.1.1", "b" * 64)
        with patch.object(MERGE, "updater", return_value=resolver), \
                patch.object(resolver, "latest", return_value=after["dependencies"]["host"]) as latest:
            MERGE.validate_artifacts(before, after, True)
            latest.assert_called_once_with(after["dependencies"]["host"], True, exact=True)
            with self.assertRaisesRegex(ValueError, "incompatible"):
                MERGE.validate_artifacts(before, self.artifact("0.2.0"), True)
            with self.assertRaisesRegex(ValueError, "checksums"):
                MERGE.validate_artifacts(before, self.artifact("0.1.1", "c" * 64), True)
            with self.assertRaisesRegex(ValueError, "immutable"):
                MERGE.validate_artifacts(before, self.artifact("0.1.0", "c" * 64), True)

    def test_dependency_identity_and_names_cannot_change(self):
        before = self.artifact()
        after = deepcopy(before)
        after["dependencies"]["host"]["repository"] = "another/host-tools"
        with self.assertRaisesRegex(ValueError, "identity"):
            MERGE.validate_artifacts(before, after, True)
        after = deepcopy(before)
        after["dependencies"]["new"] = after["dependencies"]["host"]
        with self.assertRaisesRegex(ValueError, "names"):
            MERGE.validate_artifacts(before, after, True)

    @staticmethod
    def lock(version="0.1.0", source="registry+sparse+https://raw.githubusercontent.com/idleai/host-tools/cargo-index/", checksum="a" * 64):
        return {"package": [{"name": "idle-history", "version": version, "source": source, "checksum": checksum}]}

    def test_cargo_upgrade_must_keep_compatible_versions_and_registry(self):
        MERGE.validate_cargo(self.lock(), self.lock("0.1.1", checksum="b" * 64))
        for candidate in (self.lock("0.2.0"), self.lock("0.0.9"),
                          self.lock("0.1.1", source="registry+sparse+https://raw.githubusercontent.com/idleai/other/")):
            with self.assertRaisesRegex(ValueError, "incompatible"):
                MERGE.validate_cargo(self.lock(), candidate)
        with self.assertRaisesRegex(ValueError, "immutable"):
            MERGE.validate_cargo(self.lock(), self.lock(checksum="b" * 64))

    def test_cargo_only_third_party_changes_do_not_auto_merge(self):
        before = self.lock()
        after = deepcopy(before)
        after["package"].append({"name": "unrelated", "version": "1.0.1", "source": "crates.io"})
        with self.assertRaisesRegex(ValueError, "no compatible internal"):
            MERGE.validate_cargo(before, after)


if __name__ == "__main__":
    unittest.main()
