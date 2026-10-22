import importlib.util
import subprocess
import tempfile
import unittest
from pathlib import Path

import yaml

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/asset_catalog.py"
spec = importlib.util.spec_from_file_location("asset_catalog", SCRIPT)
catalog = importlib.util.module_from_spec(spec)
spec.loader.exec_module(catalog)


class CatalogBehavior(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        base = Path(self.temp.name)
        self.root, self.remote, self.installed = base / "repo", base / "origin.git", base / "skill"
        self.root.mkdir()
        self.git("init", "-b", "main")
        self.git("config", "user.email", "catalog-test@example.invalid")
        self.git("config", "user.name", "Catalog test")
        subprocess.run(["git", "init", "--bare", str(self.remote)], check=True, capture_output=True)
        subprocess.run(["git", "-C", str(self.remote), "symbolic-ref", "HEAD", "refs/heads/main"],
                       check=True, capture_output=True)
        self.git("remote", "add", "origin", str(self.remote))
        self.write("example/Cargo.toml", '[package]\nname="example"\nversion="0.1.0"\n')
        self.write("example/src/lib.rs", 'pub fn answer() -> u8 { 42 }\n')
        self.write(catalog.SKILL + "/SKILL.md", "---\nname: create-assets\ndescription: Test\n---\n")
        self.index = {"schema": "assets.index.v1", "coverage": "complete",
                      "repository": {"url": str(self.remote), "branch": "main"},
                      "assets": [{"id": "EXAMPLE-001", "name": "example", "path": "example",
                                  "kind": "asset", "purpose": "Answer computation", "capabilities": ["answer"],
                                  "status": "source-present", "packages": [{"name": "example",
                                  "manifest": "example/Cargo.toml"}]}]}
        self.save()
        catalog.refresh(self.root)
        self.publish()
        catalog.install(self.root, self.installed)

    def write(self, path, content):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8")

    def git(self, *args):
        return catalog.git(self.root, *args)

    def save(self):
        self.write(catalog.INDEX, yaml.safe_dump(self.index, sort_keys=False))

    def publish(self):
        self.git("add", ".")
        self.git("commit", "-m", "fixture")
        self.git("push", "origin", "main")

    def codes(self):
        return {f["code"] for f in catalog.parity(self.root, [self.installed])["findings"]}

    def test_clean_publication_and_installed_copy_are_in_sync(self):
        result = catalog.parity(self.root, [self.installed])
        self.assertEqual(result["status"], "IN_SYNC")
        self.assertEqual(result["origin_revision"], self.git("rev-parse", "HEAD").decode().strip())

    def test_changed_source_with_unchanged_version_is_not_equal(self):
        self.write("example/src/lib.rs", "pub fn answer() -> u8 { 43 }\n")
        self.assertIn("CONTENT_DRIFT", self.codes())

    def test_missing_inventory_entry_and_manifest_version_drift_fail(self):
        self.write("new/Cargo.toml", '[package]\nname="new"\nversion="1.0.0"\n')
        errors = catalog.check(self.root)
        self.assertTrue(any("uncatalogued package new/Cargo.toml" in e for e in errors))
        self.write("example/Cargo.toml", '[package]\nname="example"\nversion="0.2.0"\n')
        self.assertTrue(any("version drift" in e for e in catalog.check(self.root)))

    def test_stale_generated_and_installed_inventory_are_detected(self):
        self.write(catalog.SKILL + "/references/assets-index.yaml", "stale\n")
        self.assertTrue(any("generated skill index is stale" in e for e in catalog.check(self.root)))
        catalog.refresh(self.root)
        (self.installed / "references/assets-index.yaml").write_text("stale")
        self.assertIn("SKILL_DRIFT", self.codes())

    def test_new_remote_branch_asset_is_discovered_without_checkout(self):
        self.git("checkout", "-b", "device-two")
        self.write("remote-new/Cargo.toml", '[package]\nname="remote-new"\nversion="0.1.0"\n')
        self.git("add", ".")
        self.git("commit", "-m", "second device asset")
        self.git("push", "origin", "device-two")
        self.git("checkout", "main")
        self.assertIn("UNINDEXED_ORIGIN_PACKAGE", self.codes())
        self.assertFalse((self.root / "remote-new").exists())

    def test_remote_updates_are_fetched_and_do_not_overwrite_local_work(self):
        self.write("example/src/lib.rs", "pub fn answer() -> u8 { 44 }\n")
        self.publish()
        published = self.git("rev-parse", "HEAD").decode().strip()
        self.git("checkout", "HEAD~1", "--", "example/src/lib.rs")
        result = catalog.parity(self.root, [self.installed])
        self.assertEqual(result["origin_revision"], published)
        self.assertIn("CONTENT_DRIFT", {f["code"] for f in result["findings"]})
        self.assertIn("42", (self.root / "example/src/lib.rs").read_text())

    def test_workspace_version_is_read_from_owning_manifest(self):
        self.write("Cargo.toml", '[workspace]\nmembers=["example"]\n[workspace.package]\nversion="2.3.4"\n')
        self.write("example/Cargo.toml", '[package]\nname="example"\nversion.workspace=true\n')
        catalog.refresh(self.root)
        actual = yaml.safe_load((self.root / catalog.INDEX).read_text())
        self.assertEqual(actual["assets"][0]["packages"][0]["version"], "2.3.4")

    def test_unsafe_path_and_duplicate_identity_fail(self):
        self.index["assets"].append(dict(self.index["assets"][0]))
        self.save()
        self.assertTrue(any("duplicate ID" in e for e in catalog.check(self.root)))
        self.index["assets"][0]["path"] = "../outside"
        self.save()
        self.assertTrue(any("unsafe repository path" in e for e in catalog.check(self.root)))

    def test_unavailable_origin_does_not_report_in_sync(self):
        self.git("remote", "set-url", "origin", str(self.remote.parent / "missing.git"))
        with self.assertRaises(ValueError):
            catalog.parity(self.root, [self.installed])

    def test_remote_only_source_changes_without_version_bump_invalidate_snapshot(self):
        self.git("checkout", "-b", "device-two")
        self.write("remote-new/Cargo.toml", '[package]\nname="remote-new"\nversion="0.1.0"\n')
        self.write("remote-new/src/lib.rs", "pub fn answer() -> u8 { 1 }\n")
        self.git("add", ".")
        self.git("commit", "-m", "remote package")
        self.git("push", "origin", "device-two")
        self.git("checkout", "main")
        self.index["assets"].append({"id": "REMOTE-001", "name": "remote-new", "path": "remote-new",
                                    "kind": "asset", "status": "source-present", "purpose": "Remote answer",
                                    "capabilities": ["answer"], "source_branch": "device-two",
                                    "packages": [{"name": "remote-new", "manifest": "remote-new/Cargo.toml"}]})
        self.save()
        catalog.refresh(self.root)
        self.assertEqual(catalog.check(self.root), [])
        self.git("checkout", "device-two")
        self.write("remote-new/src/lib.rs", "pub fn answer() -> u8 { 2 }\n")
        self.git("add", "remote-new/src/lib.rs")
        self.git("commit", "-m", "same version different implementation")
        self.git("push", "origin", "device-two")
        self.git("checkout", "main")
        self.assertTrue(any("source-tree drift" in error for error in catalog.check(self.root)))

    def test_extra_installed_file_is_reported(self):
        (self.installed / "obsolete.md").write_text("old rules")
        self.assertIn("SKILL_EXTRA_FILE", self.codes())

    def test_changed_canonical_branch_is_reported(self):
        self.git("push", "origin", "main:replacement")
        subprocess.run(["git", "-C", str(self.remote), "symbolic-ref", "HEAD", "refs/heads/replacement"],
                       check=True, capture_output=True)
        self.assertIn("ORIGIN_DEFAULT_BRANCH_DRIFT", self.codes())

    def test_historical_vendor_paths_keep_their_classification_after_relocation(self):
        index = {"legacy_root_paths": {"old": "assets/old"},
                 "exclusions": [{"path": "assets/old/vendor", "reason": "third party"}]}
        self.assertTrue(catalog.excluded("old/vendor/dependency/Cargo.toml", index))
        self.assertFalse(catalog.excluded("old/first-party/Cargo.toml", index))


if __name__ == "__main__":
    unittest.main()
