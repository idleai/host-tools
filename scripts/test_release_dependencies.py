"""Check fresh dependency selection, complete bundles, and reuse within one build."""

import copy
import io
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import release_dependencies as dependencies

SPEC = importlib.util.spec_from_file_location("release_watch", Path(__file__).with_name("check-release-updates.py"))
WATCH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(WATCH)


class VersionSelectionTests(unittest.TestCase):
    def test_cargo_compatibility_ranges(self):
        for requirement, version, expected in (
            ("^0.1.2", (0, 1, 3), True), ("^0.1.2", (0, 2, 0), False),
            ("0.1", (0, 1, 0), True), ("0.1", (0, 1, 99), True),
            ("0.1.2", (0, 1, 1), False), ("1.2.3", (1, 9, 0), True),
            ("1.2.3", (2, 0, 0), False), ("0.0.3", (0, 0, 4), False),
            ("0", (0, 9, 0), True), ("1", (2, 0, 0), False),
        ):
            with self.subTest(requirement=requirement, version=version):
                self.assertEqual(dependencies.matches(requirement, version), expected)
        with self.assertRaisesRegex(ValueError, "caret"):
            dependencies.matches("*", (1, 0, 0))

    @staticmethod
    def release(version, missing=None, draft=False, prerelease=False):
        tag = f"engine-v{version}"
        names = [f"{tag}-{target}.tar.gz{suffix}" for target in dependencies.PLATFORMS
                 for suffix in ("", ".sha256")]
        return {"tag_name": tag, "draft": draft, "prerelease": prerelease,
                "assets": [{"name": name, "browser_download_url": f"https://example.test/{name}"}
                           for name in names if name != missing]}

    def latest(self, releases, checksum_name=None):
        def response(request, timeout):
            filename = request.full_url.rsplit("/", 1)[1].removesuffix(".sha256")
            return io.BytesIO(f"{'a' * 64}  {checksum_name or filename}\n".encode())

        with patch.object(dependencies.subprocess, "check_output", return_value="\n".join(map(json.dumps, releases))), \
                patch.object(dependencies.urllib.request, "urlopen", side_effect=response):
            return dependencies.latest({"repository": "idleai/editchain", "package": "engine", "version": "^0.1.2"}, True)

    def test_latest_complete_compatible_release_wins(self):
        releases = [self.release("0.2.0"), self.release("0.1.10", draft=True),
                    self.release("0.1.9", prerelease=True),
                    self.release("0.1.8", missing="engine-v0.1.8-win32-x64.tar.gz.sha256"),
                    self.release("0.1.2"), self.release("0.1.3")]
        selected = self.latest(releases)
        self.assertEqual(selected["tag"], "engine-v0.1.3")
        self.assertEqual(set(selected["sha256"]), set(dependencies.PLATFORMS))

    def test_missing_compatible_release_fails(self):
        with self.assertRaisesRegex(RuntimeError, "no complete compatible"):
            self.latest([self.release("0.2.0")])

    def test_checksum_must_name_the_selected_archive(self):
        with self.assertRaisesRegex(ValueError, "checksum"):
            self.latest([self.release("0.1.3")], checksum_name="another.tar.gz")


class BuildInputTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.environment = patch.dict(os.environ)
        self.environment.start()
        self.addCleanup(self.environment.stop)
        os.environ.pop(dependencies.ENVIRONMENT, None)
        os.environ.pop("IDLE_LOCAL_RELEASE_INPUTS", None)
        self.root = Path(self.temporary.name)
        self.git("init", "-q")
        self.git("config", "user.name", "Fixture")
        self.git("config", "user.email", "fixture@example.test")
        (self.root / "Cargo.toml").write_text('[package]\nname = "fixture"\nversion = "0.1.0"\n')
        (self.root / "Cargo.lock").write_text('version = 4\n')
        (self.root / ".gitignore").write_text('/target/\n')
        self.specification = {"repository": "idleai/editchain", "package": "engine", "version": "^0.1.2"}
        self.manifest = self.root / "native-dependencies.json"
        self.manifest.write_text(json.dumps({"schema": 2, "dependencies": {"engine": self.specification}}))
        self.git("add", ".")
        self.git("commit", "-qm", "initial")
        self.selected = dict(self.specification, tag="engine-v0.1.3", sha256={platform: "a" * 64 for platform in dependencies.PLATFORMS})
        self.artifacts = {"native-dependencies.json": {"schema": 1, "dependencies": {"engine": self.selected}}}

    def git(self, *arguments):
        return subprocess.check_output(["git", *arguments], cwd=self.root, text=True).strip()

    def test_same_source_commit_can_select_a_newer_release_on_the_next_build(self):
        original = self.manifest.read_bytes()
        with patch.object(dependencies, "update_cargo") as update, \
                patch.object(dependencies, "latest", return_value=self.selected) as latest:
            first = dependencies.ensure(self.root)
            latest.return_value = dict(self.selected, tag="engine-v0.1.4")
            second = dependencies.ensure(self.root)
        self.assertEqual(update.call_count, 2)
        self.assertEqual(first["revision"], second["revision"])
        self.assertNotEqual(dependencies.fingerprint(first), dependencies.fingerprint(second))
        self.assertEqual(self.manifest.read_bytes(), original)

    def test_child_checks_reuse_the_build_selection_without_network_resolution(self):
        selected = dependencies.capture(self.root, self.artifacts)
        os.environ[dependencies.ENVIRONMENT] = str(dependencies.record_path(self.root))
        with patch.object(dependencies, "resolve", side_effect=AssertionError("unexpected new resolution")):
            self.assertEqual(dependencies.ensure(self.root), selected)

    def test_restore_recovers_the_selected_lockfile(self):
        selected = dependencies.capture(self.root, self.artifacts)
        (self.root / "Cargo.lock").write_text('version = 4\n# another build\n')
        dependencies.restore(self.root)
        self.assertEqual((self.root / "Cargo.lock").read_text(), selected["cargo_lock"])

    def test_changed_requirements_cannot_reuse_a_previous_selection(self):
        dependencies.capture(self.root, self.artifacts)
        self.manifest.write_text(self.manifest.read_text().replace("^0.1.2", "^0.2.0"))
        with self.assertRaisesRegex(ValueError, "requirements changed"):
            dependencies.restore(self.root)

    def test_windows_checkout_line_endings_reuse_the_same_requirements(self):
        manifest = self.root / "Cargo.toml"
        manifest.write_bytes(manifest.read_bytes().replace(b"\r\n", b"\n"))
        dependencies.capture(self.root, self.artifacts)
        manifest.write_bytes(manifest.read_bytes().replace(b"\n", b"\r\n"))
        dependencies.restore(self.root)

    def test_windows_locale_restores_utf8_requirements_and_lockfile(self):
        manifest = self.root / "Cargo.toml"
        manifest.write_text(manifest.read_text() + "# Requirements \u2014 shared by all hosts\n", encoding="utf-8")
        lock = 'version = 4\n# Dependencies \u2014 selected once\n'
        (self.root / "Cargo.lock").write_text(lock, encoding="utf-8")
        selected = dependencies.capture(self.root, self.artifacts)
        original_open = Path.open

        def windows_open(path, mode="r", buffering=-1, encoding=None, errors=None, newline=None):
            if "b" not in mode and encoding in (None, "locale"):
                encoding = "cp1252"
            return original_open(path, mode, buffering, encoding, errors, newline)

        with patch.object(Path, "open", windows_open):
            self.assertEqual(dependencies.restore(self.root), selected)
        self.assertEqual((self.root / "Cargo.lock").read_bytes().decode("utf-8").replace("\r\n", "\n"), lock)

    def test_selection_from_another_source_commit_is_rejected(self):
        dependencies.capture(self.root, self.artifacts)
        self.git("commit", "--allow-empty", "-qm", "next source")
        with self.assertRaisesRegex(ValueError, "different source commit"):
            dependencies.restore(self.root)

    def test_incompatible_or_incomplete_artifact_is_rejected(self):
        for change in (lambda value: value.update(tag="engine-v0.2.0"),
                       lambda value: value["sha256"].pop("win32-x64"),
                       lambda value: value.update(repository="another/repository")):
            artifacts = copy.deepcopy(self.artifacts)
            change(artifacts["native-dependencies.json"]["dependencies"]["engine"])
            dependencies.capture(self.root, artifacts)
            with self.assertRaises(ValueError):
                dependencies.restore(self.root)

    def test_local_native_bundle_is_usable_only_for_explicit_integration_checks(self):
        selected = dict(self.selected, local_platform="linux-x64", sha256={"linux-x64": "a" * 64})
        artifacts = {"native-dependencies.json": {"schema": 1, "dependencies": {"engine": selected}}}
        dependencies.capture(self.root, artifacts)
        with self.assertRaisesRegex(ValueError, "explicit integration"):
            dependencies.restore(self.root)
        with patch.dict(os.environ, {"IDLE_LOCAL_RELEASE_INPUTS": "1"}):
            dependencies.restore(self.root)

    def test_lock_changes_after_resolution_fail_before_building(self):
        dependencies.capture(self.root, self.artifacts)
        os.environ[dependencies.ENVIRONMENT] = str(dependencies.record_path(self.root))
        (self.root / "Cargo.lock").write_text('version = 4\n# unexpected change\n')
        with self.assertRaisesRegex(ValueError, "Cargo.lock changed"):
            dependencies.ensure(self.root)


class ReleaseWatchTests(unittest.TestCase):
    @staticmethod
    def run_record(status="completed", conclusion="success"):
        return {"id": 42, "event": "workflow_dispatch", "status": status, "conclusion": conclusion}

    def test_running_main_build_does_not_start_another(self):
        with patch.dict(os.environ, {"GITHUB_REPOSITORY": "idleai/fixture"}), \
                patch.object(WATCH.dependencies, "command", return_value="a" * 40), \
                patch.object(WATCH, "api", return_value={"workflow_runs": [self.run_record("in_progress")]}), \
                patch.object(WATCH.dependencies, "resolve") as resolve, \
                patch.object(WATCH.subprocess, "run") as run:
            WATCH.main()
        resolve.assert_not_called()
        run.assert_not_called()

    def test_new_dependencies_dispatch_ci_without_creating_a_pull_request(self):
        with patch.dict(os.environ, {"GITHUB_REPOSITORY": "idleai/fixture"}), \
                patch.object(WATCH.dependencies, "command", return_value="a" * 40), \
                patch.object(WATCH, "api", return_value={"workflow_runs": []}), \
                patch.object(WATCH.dependencies, "resolve", return_value={"cargo_lock": "new", "artifacts": {}}), \
                patch.object(WATCH.subprocess, "run") as run:
            WATCH.main()
        run.assert_called_once_with(["gh", "workflow", "run", "ci.yml", "--repo", "idleai/fixture", "--ref", "main"], check=True)

    def test_same_selection_is_not_retried_until_it_changes_even_after_a_failure(self):
        record = {"cargo_lock": "selected", "artifacts": {}}

        def download(arguments, check):
            directory = Path(arguments[arguments.index("--dir") + 1])
            (directory / "released-dependencies.json").write_text(json.dumps(record))

        responses = [{"workflow_runs": [self.run_record(conclusion="failure")]},
                     {"artifacts": [{"name": "released-dependencies", "expired": False}]}]
        with patch.dict(os.environ, {"GITHUB_REPOSITORY": "idleai/fixture"}), \
                patch.object(WATCH.dependencies, "command", return_value="a" * 40), \
                patch.object(WATCH, "api", side_effect=responses), \
                patch.object(WATCH.dependencies, "resolve", return_value=record), \
                patch.object(WATCH.subprocess, "run", side_effect=download) as run:
            WATCH.main()
        self.assertEqual(run.call_count, 1)
        self.assertEqual(run.call_args.args[0][:3], ["gh", "run", "download"])


if __name__ == "__main__":
    unittest.main()
