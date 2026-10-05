"""Exercise release retries and concurrent changes using local Git repositories."""

import contextlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("release_automation", Path(__file__).with_name("release-automation.py"))
AUTOMATION = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AUTOMATION)


class ReleasePreparationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.remote = self.root / "remote.git"
        self.checkout = self.root / "checkout"
        self.run_git(self.root, "init", "--bare", str(self.remote))
        self.run_git(self.root, "clone", str(self.remote), str(self.checkout))
        self.run_git(self.checkout, "checkout", "-b", "main")
        self.run_git(self.checkout, "config", "user.name", "Fixture")
        self.run_git(self.checkout, "config", "user.email", "fixture@example.test")
        (self.checkout / "Cargo.toml").write_text('[package]\nname = "fixture"\nversion = "0.1.0"\n')
        (self.checkout / "Cargo.lock").write_text('version = 4\n')
        (self.checkout / "source.rs").write_text("original\n")
        self.commit("initial")
        self.run_git(self.checkout, "tag", "fixture-v0.1.0")
        self.run_git(self.checkout, "push", "origin", "main", "--tags")
        self.source = self.run_git(self.checkout, "rev-parse", "HEAD")
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.release_command("from pathlib import Path\np = Path('Cargo.toml')\np.write_text(p.read_text().replace('0.1.0', '0.1.1'))\n")

    @staticmethod
    def run_git(directory, *arguments):
        return subprocess.check_output(["git", *arguments], cwd=directory, text=True, stderr=subprocess.DEVNULL).strip()

    def commit(self, message):
        self.run_git(self.checkout, "add", ".")
        self.run_git(self.checkout, "commit", "-m", message)

    def release_command(self, code):
        executable = self.bin / "release-plz"
        executable.write_text("#!/usr/bin/env python3\n" + code)
        executable.chmod(0o755)

    def prepare(self, source=None, version="0.1.1", inputs=None):
        with contextlib.chdir(self.checkout), patch.dict(os.environ, {"PATH": f"{self.bin}:{os.environ['PATH']}"}), \
                patch.object(AUTOMATION, "packages", return_value=[{"name": "fixture", "version": version}]), \
                patch.object(AUTOMATION, "dependency_inputs", side_effect=inputs or (lambda: None)), \
                patch.object(AUTOMATION, "record_dependencies"), \
                patch.object(AUTOMATION, "output") as output:
            AUTOMATION.prepare(source or self.source)
            return output.call_args.args[1]

    def test_version_commit_is_pushed_once_and_retry_reuses_it(self):
        revision = self.prepare()
        self.assertNotEqual(revision, self.source)
        self.assertEqual(self.run_git(self.remote, "rev-parse", "main"), revision)
        self.assertEqual(self.run_git(self.checkout, "rev-parse", "HEAD^"), self.source)
        self.assertEqual(self.prepare(), revision)
        self.assertEqual(self.run_git(self.remote, "rev-parse", "main"), revision)

    def test_superseded_source_cannot_publish(self):
        (self.checkout / "source.rs").write_text("a newer change\n")
        self.commit("newer main")
        self.run_git(self.checkout, "push", "origin", "main")
        self.assertEqual(self.prepare(), "")
        self.assertEqual((self.checkout / "Cargo.toml").read_text().count("0.1.0"), 1)

    def test_latest_dependencies_are_committed_with_the_version_without_a_dependency_pr(self):
        lock = 'version = 4\n[[package]]\nname = "engine"\nversion = "0.1.3"\n'

        def inputs():
            (self.checkout / "Cargo.lock").write_text(lock)
            return {"artifacts": {}}

        revision = self.prepare(inputs=inputs)
        self.assertEqual(self.run_git(self.remote, "show", f"{revision}:Cargo.lock"), lock.strip())
        self.assertEqual(self.run_git(self.checkout, "rev-parse", "HEAD^"), self.source)

    def test_retry_restores_the_prepared_dependencies_before_reusing_the_commit(self):
        revision = self.prepare()
        committed = (self.checkout / "Cargo.lock").read_text()
        self.run_git(self.checkout, "checkout", "--detach", self.source)

        def inputs():
            (self.checkout / "Cargo.lock").write_text('version = 4\n# a newer dependency\n')
            return {"artifacts": {}}

        self.assertEqual(self.prepare(inputs=inputs), revision)
        self.assertEqual((self.checkout / "Cargo.lock").read_text(), committed)

    def test_non_metadata_change_is_rejected_before_push(self):
        self.release_command("from pathlib import Path\nPath('source.rs').write_text('unexpected')\n")
        with self.assertRaisesRegex(ValueError, "outside release metadata"):
            self.prepare()
        self.assertEqual(self.run_git(self.remote, "rev-parse", "main"), self.source)

    def test_parallel_push_cannot_be_overwritten(self):
        other = self.root / "other"
        self.run_git(self.root, "clone", "-b", "main", str(self.remote), str(other))
        self.run_git(other, "config", "user.name", "Fixture")
        self.run_git(other, "config", "user.email", "fixture@example.test")
        (other / "source.rs").write_text("concurrent main\n")
        self.run_git(other, "commit", "-am", "concurrent change")
        concurrent = self.run_git(other, "rev-parse", "HEAD")
        self.release_command(f"import subprocess\nfrom pathlib import Path\nsubprocess.run(['git', '-C', {str(other)!r}, 'push', 'origin', 'main'], check=True)\np = Path('Cargo.toml')\np.write_text(p.read_text().replace('0.1.0', '0.1.1'))\n")
        with self.assertRaises(subprocess.CalledProcessError):
            self.prepare()
        self.assertEqual(self.run_git(self.remote, "rev-parse", "main"), concurrent)

    def test_docs_only_commit_does_not_need_another_release(self):
        (self.checkout / "notes.txt").write_text("documentation\n")
        self.commit("docs")
        self.run_git(self.checkout, "push", "origin", "main")
        source = self.run_git(self.checkout, "rev-parse", "HEAD")
        self.release_command("pass\n")
        self.assertEqual(self.prepare(source, version="0.1.0"), "")

    def test_manual_version_bump_can_publish_without_another_commit(self):
        (self.checkout / "Cargo.toml").write_text('[package]\nname = "fixture"\nversion = "0.2.0"\n')
        self.commit("breaking release")
        self.run_git(self.checkout, "push", "origin", "main")
        source = self.run_git(self.checkout, "rev-parse", "HEAD")
        self.release_command("pass\n")
        self.assertEqual(self.prepare(source, version="0.2.0"), source)

    def test_artifact_retry_selects_draft_and_leaves_public_release_unchanged(self):
        self.prepare()
        self.run_git(self.checkout, "tag", "fixture-v0.1.1")
        original = AUTOMATION.command
        for draft in (True, False):
            def command(*arguments):
                if arguments[:3] == ("gh", "release", "view"):
                    return json.dumps({"isDraft": draft})
                return original(*arguments)
            with contextlib.chdir(self.checkout), \
                    patch.object(AUTOMATION, "packages", return_value=[{"name": "fixture", "version": "0.1.1"}]), \
                    patch.object(AUTOMATION, "command", side_effect=command), patch.object(AUTOMATION, "output") as output:
                AUTOMATION.artifact("fixture")
                output.assert_called_once_with("tag", "fixture-v0.1.1" if draft else "")

    def test_missing_artifact_tag_fails_instead_of_reporting_publication_success(self):
        with contextlib.chdir(self.checkout), \
                patch.object(AUTOMATION, "packages", return_value=[{"name": "fixture", "version": "0.1.1"}]):
            with self.assertRaisesRegex(ValueError, "did not create the artifact tag"):
                AUTOMATION.artifact("fixture")

    def test_unchanged_artifact_from_an_older_commit_is_not_rebuilt(self):
        (self.checkout / "source.rs").write_text("another package changed\n")
        self.commit("another package")
        with contextlib.chdir(self.checkout), \
                patch.object(AUTOMATION, "packages", return_value=[{"name": "fixture", "version": "0.1.0"}]), \
                patch.object(AUTOMATION, "output") as output:
            AUTOMATION.artifact("fixture")
            output.assert_called_once_with("tag", "")

    def test_completed_release_does_not_repeat_for_a_consumer_test_update(self):
        package = {"name": "fixture", "version": "0.1.0"}
        for draft, recorded, expected in ((False, True, False), (True, True, True), (False, False, True)):
            release = {"isDraft": draft, "assets": [{"name": "released-dependencies.json"}] if recorded else []}
            response = subprocess.CompletedProcess([], 0, json.dumps(release), "")
            with patch.object(AUTOMATION, "tag_commit", return_value=self.source), \
                    patch.object(AUTOMATION.subprocess, "run", return_value=response):
                self.assertEqual(AUTOMATION.pending(package, self.source), expected)

    def test_tag_created_before_a_failed_release_can_resume(self):
        response = subprocess.CompletedProcess([], 1, "", "release not found")
        with patch.object(AUTOMATION, "tag_commit", return_value=self.source), \
                patch.object(AUTOMATION.subprocess, "run", return_value=response):
            self.assertTrue(AUTOMATION.pending({"name": "fixture", "version": "0.1.0"}, self.source))

    def test_release_lookup_failure_is_not_treated_as_a_missing_release(self):
        response = subprocess.CompletedProcess([], 1, "", "HTTP 503: Service Unavailable")
        with patch.object(AUTOMATION, "tag_commit", return_value=self.source), \
                patch.object(AUTOMATION.subprocess, "run", return_value=response):
            with self.assertRaises(subprocess.CalledProcessError):
                AUTOMATION.pending({"name": "fixture", "version": "0.1.0"}, self.source)


class ReleaseConfigurationTests(unittest.TestCase):
    def test_tag_only_packages_do_not_require_a_github_release(self):
        with tempfile.TemporaryDirectory() as temporary, contextlib.chdir(temporary):
            Path("release-plz.toml").write_text(
                '[[package]]\nname = "disabled"\nrelease = false\n'
                '[[package]]\nname = "bundled"\ngit_release_enable = false\n', encoding="utf-8")
            packages = [{"name": name} for name in ("published", "disabled", "bundled")]
            with patch.object(AUTOMATION, "command", return_value=json.dumps({"packages": packages})):
                self.assertEqual(AUTOMATION.packages(), [{"name": "published"}])

    def test_every_release_has_a_crate_or_native_bundle_to_publish(self):
        root = Path(__file__).resolve().parent.parent
        native = json.loads((root / "native-release.json").read_text())
        native_package = native["tag_prefix"].removesuffix("-v")
        with contextlib.chdir(root):
            packages = AUTOMATION.packages()
        self.assertIn(native_package, {package["name"] for package in packages})
        for package in packages:
            with self.subTest(package=package["name"]):
                self.assertTrue(package["publish"] or package["name"] == native_package,
                                "a release must publish a Cargo archive or own the native bundle")


if __name__ == "__main__":
    unittest.main()
