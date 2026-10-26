import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path

import yaml

GOV = Path(__file__).resolve().parents[1]
SYNC = GOV / "skills/create-assets/scripts/asset_sync.py"
spec = importlib.util.spec_from_file_location("asset_sync", SYNC)
sync = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sync)
spec = importlib.util.spec_from_file_location("sync_test_catalog", GOV / "scripts/asset_catalog.py")
catalog = importlib.util.module_from_spec(spec)
spec.loader.exec_module(catalog)


class DeviceSynchronization(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.root, self.remote, self.installed = self.base / "device-one", self.base / "origin.git", self.base / "skill"
        self.root.mkdir()
        self.git("init", "-b", "main")
        self.git("config", "user.name", "Sync test")
        self.git("config", "user.email", "sync-test@example.invalid")
        subprocess.run(["git", "init", "--bare", str(self.remote)], capture_output=True, check=True)
        sync.run(self.remote, "symbolic-ref", "HEAD", "refs/heads/main")
        self.git("remote", "add", "origin", str(self.remote))
        self.write(".gitignore", ".local/\n**/__pycache__/\n")
        self.write("assets/example/Cargo.toml", '[package]\nname="example"\nversion="0.1.0"\n')
        self.write("assets/example/src/lib.rs", "pub fn answer() -> u8 { 42 }\n")
        self.write("assets/example/README.md", "The answer must be 42.\n")
        self.write("unrelated.txt", "original\n")
        self.write(catalog.SKILL + "/SKILL.md", "---\nname: create-assets\ndescription: Test\n---\nold rules\n")
        for source, destination in [(SYNC, self.root / catalog.SKILL / "scripts/asset_sync.py"),
                                    (GOV / "scripts/asset_catalog.py", self.root / "repo-governance/scripts/asset_catalog.py")]:
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, destination)
        index = {"schema": "assets.index.v1", "coverage": "complete",
                 "repository": {"url": str(self.remote), "branch": "main", "asset_root": "assets"},
                 "assets": [{"id": "EXAMPLE-001", "name": "example", "path": "assets/example",
                             "kind": "asset", "purpose": "Answer", "capabilities": ["answer"],
                             "status": "source-present", "packages": [{"name": "example",
                             "manifest": "assets/example/Cargo.toml"}]}]}
        self.write(catalog.INDEX, yaml.safe_dump(index, sort_keys=False))
        catalog.refresh(self.root)
        self.git("add", ".")
        self.git("commit", "-m", "initial")
        self.git("push", "origin", "main")
        catalog.install(self.root, self.installed)
        sync.before_use(self.root, [self.installed], str(self.remote))
        self.second = self.base / "device-two"
        subprocess.run(["git", "clone", str(self.remote), str(self.second)], capture_output=True, check=True)
        sync.run(self.second, "config", "user.name", "Second device")
        sync.run(self.second, "config", "user.email", "second@example.invalid")

    def git(self, *args):
        return sync.run(self.root, *args)

    def write(self, path, content):
        destination = self.root / path
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(content, encoding="utf-8")

    def update_other_device(self, path, content):
        target = self.second / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8")
        sync.run(self.second, "add", path)
        sync.run(self.second, "commit", "-m", "second device change")
        sync.run(self.second, "push", "origin", "main")

    def plan(self, code=None):
        return {"assets": [{"id": "EXAMPLE-001", "paths": ["assets/example/src/lib.rs"], "checks": [{
            "argv": [sys.executable, "-c", code or "from pathlib import Path; assert '43' in Path('src/lib.rs').read_text()"],
            "authority": "assets/example/README.md"}]}]}

    def test_fetches_latest_rules_and_preserves_unrelated_dirty_and_staged_work(self):
        self.write("unrelated.txt", "private staged work\n")
        self.git("add", "unrelated.txt")
        self.write("private.txt", "private untracked\n")
        index_before = self.git("ls-files", "-s", "unrelated.txt")
        self.update_other_device(catalog.SKILL + "/SKILL.md", "---\nname: create-assets\ndescription: Test\n---\nnew rules\n")
        result = sync.before_use(self.root, [self.installed], str(self.remote))
        self.assertEqual(result["status"], "UPDATED")
        self.assertIn("new rules", (self.installed / "SKILL.md").read_text())
        self.assertEqual(index_before, self.git("ls-files", "-s", "unrelated.txt"))
        self.assertEqual((self.root / "private.txt").read_text(), "private untracked\n")

    def test_incoming_conflict_preserves_local_files_head_and_installed_skill(self):
        self.write("unrelated.txt", "local edit\n")
        head = self.git("rev-parse", "HEAD")
        installed = (self.installed / "SKILL.md").read_bytes()
        self.update_other_device("unrelated.txt", "remote edit\n")
        with self.assertRaises(ValueError):
            sync.before_use(self.root, [self.installed], str(self.remote))
        self.assertEqual(self.git("rev-parse", "HEAD"), head)
        self.assertEqual((self.installed / "SKILL.md").read_bytes(), installed)
        self.assertEqual((self.root / "unrelated.txt").read_text(), "local edit\n")

    def test_local_skill_edits_stop_before_installation(self):
        (self.installed / "SKILL.md").write_text("local customization\n")
        with self.assertRaisesRegex(ValueError, "local skill edits preserved"):
            sync.before_use(self.root, [self.installed], str(self.remote))
        self.assertIn("local customization", (self.installed / "SKILL.md").read_text())

    def edit_local_catalog(self):
        path = self.root / catalog.INDEX
        index = yaml.safe_load(path.read_text(encoding="utf-8"))
        index["assets"][0]["capabilities"].append("unpublished local capability")
        path.write_text(yaml.safe_dump(index, sort_keys=False), encoding="utf-8")
        catalog.refresh(self.root)
        # Exercise the existing normalized hash with different platform line endings.
        path.write_bytes(path.read_bytes().replace(b"\r\n", b"\n").replace(b"\n", b"\r\n"))
        generated = self.root / catalog.SKILL / "references/assets-index.yaml"
        return {path: path.read_bytes(), generated: generated.read_bytes()}

    def publish_absorbed_catalog(self, *, conflicting_field=False, incoming_unrelated=False):
        index = yaml.safe_load((self.root / catalog.INDEX).read_text(encoding="utf-8"))
        if conflicting_field:
            index["assets"][0]["purpose"] = "remote field choice"
        index["assets"].append({"id": "RSS-TEST", "name": "rss", "path": "assets/rss",
                                "kind": "asset", "purpose": "RSS test additions",
                                "capabilities": [f"RSS addition {number}" for number in range(8)],
                                "status": "source-present", "packages": [{"name": "rss-test",
                                "manifest": "assets/rss/Cargo.toml"}]})
        for relative, content in {
            catalog.INDEX: yaml.safe_dump(index, sort_keys=False),
            "assets/rss/Cargo.toml": '[package]\nname="rss-test"\nversion="0.1.0"\n',
            "assets/rss/src/lib.rs": "pub fn rss() {}\n",
            "assets/rss/README.md": "RSS test asset.\n",
            "assets/example/src/lib.rs": "pub fn answer() -> u8 { 43 }\n",
            "assets/example/archive/old.txt": "already present locally\n",
        }.items():
            path = self.second / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding="utf-8")
        if incoming_unrelated:
            (self.second / "unrelated.txt").write_text("incoming change\n", encoding="utf-8")
        catalog.refresh(self.second)
        sync.run(self.second, "add", ".")
        sync.run(self.second, "commit", "-m", "absorb local capability and add RSS")
        sync.run(self.second, "push", "origin", "main")
        return sync.run(self.second, "rev-parse", "HEAD").decode().strip()

    def old_publication_receipt(self):
        path = self.root / ".local/asset-publication/latest.json"
        self.write(path.relative_to(self.root), '{"base":"earlier-base","revision":"earlier-revision","paths":[]}\n')
        return path, path.read_bytes()

    def assert_preserved_catalog(self, result, originals):
        backup = Path(result["catalog_preservation"])
        for path, content in originals.items():
            self.assertEqual((backup / path.relative_to(self.root)).read_bytes(), content)
            self.assertEqual(path.read_bytes(), self.git("show", "HEAD:" + path.relative_to(self.root).as_posix()))

    def test_matching_local_catalog_projection_is_preserved_without_installing_unpublished_inventory(self):
        preserved = self.edit_local_catalog()
        self.git("add", catalog.INDEX)
        index_before = self.git("ls-files", "-s")
        official = self.git("show", "HEAD:assets-index.yaml")
        result = sync.before_use(self.root, [self.root / catalog.SKILL, self.installed], str(self.remote))
        self.assertEqual(result["status"], "UPDATED")
        self.assertEqual(result["local_catalog"], "UNPUBLISHED")
        self.assertEqual(result["source_synchronization"], "NOT_SYNCHRONIZED")
        for path, content in preserved.items():
            self.assertEqual(path.read_bytes(), content)
        self.assertEqual(self.git("ls-files", "-s"), index_before)
        self.assertEqual((self.installed / "references/assets-index.yaml").read_bytes(), official)

    def test_fresh_remote_skill_installs_while_matching_local_catalog_projection_remains(self):
        preserved = self.edit_local_catalog()
        self.update_other_device(catalog.SKILL + "/SKILL.md", "---\nname: create-assets\ndescription: Test\n---\nfresh official rules\n")
        result = sync.before_use(self.root, [self.root / catalog.SKILL, self.installed], str(self.remote))
        self.assertEqual(result["local_catalog"], "UNPUBLISHED")
        self.assertEqual(self.git("rev-parse", "HEAD"), self.git("rev-parse", "origin/main"))
        self.assertIn("fresh official rules", (self.installed / "SKILL.md").read_text())
        for path, content in preserved.items():
            self.assertEqual(path.read_bytes(), content)
        self.assertEqual((self.installed / "references/assets-index.yaml").read_bytes(),
                         self.git("show", "origin/main:assets-index.yaml"))
        self.assertNotIn(b"unpublished local capability", (self.installed / "references/assets-index.yaml").read_bytes())

    def test_mismatched_generated_inventory_stops_with_local_catalog_preserved(self):
        preserved = self.edit_local_catalog()
        generated = self.root / catalog.SKILL / "references/assets-index.yaml"
        generated.write_bytes(b"independent generated-copy edit\n")
        preserved[generated] = generated.read_bytes()
        installed = (self.installed / "references/assets-index.yaml").read_bytes()
        head = self.git("rev-parse", "HEAD")
        with self.assertRaisesRegex(ValueError, "generated inventory mismatch"):
            sync.before_use(self.root, [self.root / catalog.SKILL, self.installed], str(self.remote))
        self.assertEqual(self.git("rev-parse", "HEAD"), head)
        self.assertEqual((self.installed / "references/assets-index.yaml").read_bytes(), installed)
        for path, content in preserved.items():
            self.assertEqual(path.read_bytes(), content)

    def test_remote_catalog_conflict_preserves_local_catalog_head_index_and_installation(self):
        preserved = self.edit_local_catalog()
        self.git("add", catalog.INDEX)
        head, index_before = self.git("rev-parse", "HEAD"), self.git("ls-files", "-s")
        installed = (self.installed / "references/assets-index.yaml").read_bytes()
        remote_catalog = self.second / catalog.INDEX
        index = yaml.safe_load(remote_catalog.read_text(encoding="utf-8"))
        index["assets"][0]["capabilities"].append("remote capability")
        remote_catalog.write_text(yaml.safe_dump(index, sort_keys=False), encoding="utf-8")
        catalog.refresh(self.second)
        sync.run(self.second, "add", catalog.INDEX, catalog.SKILL + "/references/assets-index.yaml")
        sync.run(self.second, "commit", "-m", "remote catalog update")
        sync.run(self.second, "push", "origin", "main")
        with self.assertRaisesRegex(ValueError, "incoming catalog update conflicts"):
            sync.before_use(self.root, [self.root / catalog.SKILL, self.installed], str(self.remote))
        self.assertEqual(self.git("rev-parse", "HEAD"), head)
        self.assertEqual(self.git("ls-files", "-s"), index_before)
        self.assertEqual((self.installed / "references/assets-index.yaml").read_bytes(), installed)
        for path, content in preserved.items():
            self.assertEqual(path.read_bytes(), content)

    def test_local_repository_control_edit_still_stops_with_matching_dirty_catalog(self):
        preserved = self.edit_local_catalog()
        control = self.root / catalog.SKILL / "SKILL.md"
        control.write_bytes(b"local authored control edit\n")
        with self.assertRaisesRegex(ValueError, "local repository skill change preserved: SKILL.md"):
            sync.before_use(self.root, [self.root / catalog.SKILL, self.installed], str(self.remote))
        self.assertEqual(control.read_bytes(), b"local authored control edit\n")
        for path, content in preserved.items():
            self.assertEqual(path.read_bytes(), content)

    def test_own_catalog_publication_is_adopted_and_preserves_unrelated_staged_work(self):
        self.edit_local_catalog()
        self.write("assets/example/src/lib.rs", "pub fn answer() -> u8 { 43 }\n")
        self.write("unrelated.txt", "private staged work\n")
        self.git("add", "unrelated.txt")
        index_before = (self.root / ".git/index").read_bytes()
        unrelated = self.git("ls-files", "-s", "unrelated.txt")
        published = sync.publish(self.root, self.plan(), "publish capability and code", str(self.remote))
        self.assertEqual((self.root / ".git/index").read_bytes(), index_before)
        result = sync.before_use(self.root, [self.root / catalog.SKILL, self.installed], str(self.remote))
        self.assertEqual(self.git("rev-parse", "HEAD").decode().strip(), published["revision"])
        self.assertEqual(result["local_catalog"], "CURRENT")
        self.assertEqual(self.git("ls-files", "-s", "unrelated.txt"), unrelated)
        self.assertEqual(self.git("diff", "--cached", "--name-only").decode().strip(), "unrelated.txt")
        self.assertEqual(self.git("diff", "HEAD", "--", catalog.INDEX,
                                  catalog.SKILL + "/references/assets-index.yaml", "assets/example/src/lib.rs"), b"")
        self.assertIn(b"unpublished local capability", (self.installed / "references/assets-index.yaml").read_bytes())

    def test_staged_only_catalog_edit_is_reported_and_preserves_index_and_working_bytes(self):
        self.edit_local_catalog()
        self.git("add", catalog.INDEX)
        paths = [catalog.INDEX, catalog.SKILL + "/references/assets-index.yaml"]
        for path in paths:
            (self.root / path).write_bytes(self.git("show", "HEAD:" + path))
        working = {path: (self.root / path).read_bytes() for path in paths}
        index_before = (self.root / ".git/index").read_bytes()
        result = sync.before_use(self.root, [self.root / catalog.SKILL, self.installed], str(self.remote))
        self.assertEqual(result["local_catalog"], "UNPUBLISHED")
        self.assertEqual(result["source_synchronization"], "NOT_SYNCHRONIZED")
        self.assertEqual((self.root / ".git/index").read_bytes(), index_before)
        for path, content in working.items():
            self.assertEqual((self.root / path).read_bytes(), content)

    def test_indexed_inventory_matches_indexed_or_working_catalog_matrix(self):
        self.edit_local_catalog()
        canonical = catalog.INDEX
        generated = catalog.SKILL + "/references/assets-index.yaml"
        working = {path: (self.root / path).read_bytes() for path in (canonical, generated)}
        head = self.git("rev-parse", "HEAD")
        staged_source = yaml.safe_load(working[canonical])
        staged_source["assets"][0]["capabilities"].append("coherent staged catalog snapshot")
        snapshots = {"A": self.git("show", "HEAD:" + canonical),
                     "B": working[canonical],
                     "C": yaml.safe_dump(staged_source, sort_keys=False).encode("utf-8")}
        installed = (self.installed / "references/assets-index.yaml").read_bytes()
        for catalog_name in snapshots:
            for inventory_name in snapshots:
                with self.subTest(indexed_catalog=catalog_name, indexed_inventory=inventory_name):
                    # Reset only the synthetic test index; working snapshots stay explicitly controlled.
                    self.git("read-tree", "HEAD")
                    (self.root / canonical).write_bytes(snapshots[catalog_name])
                    (self.root / generated).write_bytes(snapshots[inventory_name])
                    self.git("add", canonical, generated)
                    for path, content in working.items():
                        (self.root / path).write_bytes(content)
                    self.write("unrelated.txt", "unrelated staged work\n")
                    self.git("add", "unrelated.txt")
                    index_before = (self.root / ".git/index").read_bytes()
                    # Unchanged inventory A needs no new pairing. Changed B/C must match a source.
                    allowed = inventory_name == "A" or inventory_name in (catalog_name, "B")
                    if allowed:
                        result = sync.before_use(self.root, [self.root / catalog.SKILL, self.installed], str(self.remote))
                        self.assertEqual(result["local_catalog"], "UNPUBLISHED")
                    else:
                        with self.assertRaisesRegex(ValueError, "staged generated inventory mismatch"):
                            sync.before_use(self.root, [self.root / catalog.SKILL, self.installed], str(self.remote))
                    self.assertEqual((self.root / ".git/index").read_bytes(), index_before)
                    self.assertEqual(self.git("rev-parse", "HEAD"), head)
                    self.assertEqual((self.installed / "references/assets-index.yaml").read_bytes(), installed)
                    for path, content in working.items():
                        self.assertEqual((self.root / path).read_bytes(), content)

    def test_staged_generated_inventory_deletion_stops_and_preserves_both_states(self):
        working = self.edit_local_catalog()
        generated = catalog.SKILL + "/references/assets-index.yaml"
        self.git("rm", "--cached", generated)
        index_before = (self.root / ".git/index").read_bytes()
        installed = (self.installed / "references/assets-index.yaml").read_bytes()
        head = self.git("rev-parse", "HEAD")
        with self.assertRaises(ValueError):
            sync.before_use(self.root, [self.root / catalog.SKILL, self.installed], str(self.remote))
        self.assertEqual((self.root / ".git/index").read_bytes(), index_before)
        self.assertEqual(self.git("rev-parse", "HEAD"), head)
        self.assertEqual((self.installed / "references/assets-index.yaml").read_bytes(), installed)
        for path, content in working.items():
            self.assertEqual(path.read_bytes(), content)

    def test_staged_only_authored_control_edit_stops_without_mutating_index_or_working_file(self):
        path = catalog.SKILL + "/SKILL.md"
        original = (self.root / path).read_bytes()
        self.write(path, "staged authored customization\n")
        self.git("add", path)
        (self.root / path).write_bytes(original)
        index_before = (self.root / ".git/index").read_bytes()
        installed = (self.installed / "SKILL.md").read_bytes()
        with self.assertRaisesRegex(ValueError, "staged repository skill change preserved"):
            sync.before_use(self.root, [self.root / catalog.SKILL, self.installed], str(self.remote))
        self.assertEqual((self.root / ".git/index").read_bytes(), index_before)
        self.assertEqual((self.root / path).read_bytes(), original)
        self.assertEqual((self.installed / "SKILL.md").read_bytes(), installed)

    @unittest.skipUnless(sys.platform == "win32", "OS-enforced catalog transaction requires Windows")
    def test_matching_remote_catalog_without_own_publication_receipt_is_independently_adopted(self):
        preserved = self.edit_local_catalog()
        for path in (catalog.INDEX, catalog.SKILL + "/references/assets-index.yaml"):
            (self.second / path).write_bytes((self.root / path).read_bytes())
        sync.run(self.second, "add", catalog.INDEX, catalog.SKILL + "/references/assets-index.yaml")
        sync.run(self.second, "commit", "-m", "independent matching catalog update")
        sync.run(self.second, "push", "origin", "main")
        result = sync.before_use(self.root, [self.installed], str(self.remote))
        self.assertEqual(self.git("rev-parse", "HEAD"), self.git("rev-parse", "origin/main"))
        self.assertEqual(result["local_catalog"], "CURRENT")
        self.assert_preserved_catalog(result, preserved)

    @unittest.skipUnless(sys.platform == "win32", "OS-enforced catalog transaction requires Windows")
    def test_absorbed_catalog_adopts_exact_incoming_overlaps_and_keeps_partial_staging(self):
        preserved = self.edit_local_catalog()
        self.write("assets/example/src/lib.rs", "pub fn answer() -> u8 { 43 }\n")
        self.write("assets/example/archive/old.txt", "already present locally\n")
        self.write("unrelated.txt", "staged private work\n")
        self.git("add", "unrelated.txt")
        self.write("unrelated.txt", "unstaged private work\n")
        self.write("private.txt", "untracked private work\n")
        staged = self.git("ls-files", "-s", "unrelated.txt")
        receipt, receipt_bytes = self.old_publication_receipt()
        incoming = self.publish_absorbed_catalog()
        actual_run, denied_writes = sync.run, []
        def git_under_catalog_read_guard(root, *args, env=None, data=None):
            if Path(root) == self.root and args[:3] == ("merge", "--ff-only", "--no-edit") and env:
                with self.assertRaises(OSError):
                    (self.root / catalog.INDEX).write_bytes(b"foreign edit during Git adoption\n")
                denied_writes.append(True)
            return actual_run(root, *args, env=env, data=data)
        with patch.object(sync, "run", git_under_catalog_read_guard):
            result = sync.before_use(self.root, [self.root / catalog.SKILL, self.installed], str(self.remote))
        self.assertEqual(denied_writes, [True])
        self.assertEqual(self.git("rev-parse", "HEAD").decode().strip(), incoming)
        self.assertEqual(result["local_catalog"], "CURRENT")
        self.assert_preserved_catalog(result, preserved)
        self.assertEqual(staged, self.git("ls-files", "-s", "unrelated.txt"))
        self.assertEqual((self.root / "unrelated.txt").read_text(), "unstaged private work\n")
        self.assertEqual((self.root / "private.txt").read_text(), "untracked private work\n")
        self.assertIn(b"private.txt", self.git("ls-files", "--others", "--exclude-standard"))
        self.assertEqual((self.root / "assets/example/src/lib.rs").read_text(), "pub fn answer() -> u8 { 43 }\n")
        self.assertEqual((self.root / "assets/example/archive/old.txt").read_text(), "already present locally\n")
        self.assertEqual(receipt.read_bytes(), receipt_bytes)
        self.assertEqual((self.installed / "references/assets-index.yaml").read_bytes(), self.git("show", "HEAD:assets-index.yaml"))
        self.assertEqual(len(yaml.safe_load((self.root / catalog.INDEX).read_text())["assets"][1]["capabilities"]), 8)

    @unittest.skipUnless(sys.platform == "win32", "OS-enforced catalog transaction requires Windows")
    def test_absorbed_staged_catalog_addition_and_remote_rss_preserve_each_index_projection(self):
        originals = self.edit_local_catalog()
        self.git("add", catalog.INDEX)  # The generated copy remains unstaged, as on a partially staged device.
        indexed = {path: self.git("show", ":" + path) for path in
                   [catalog.INDEX, catalog.SKILL + "/references/assets-index.yaml"]}
        self.assertNotEqual(indexed[catalog.INDEX], indexed[catalog.SKILL + "/references/assets-index.yaml"])
        original_index = (self.root / ".git/index").read_bytes()
        receipt, receipt_bytes = self.old_publication_receipt()
        incoming = self.publish_absorbed_catalog()
        result = sync.before_use(self.root, [self.installed], str(self.remote))
        self.assertEqual(self.git("rev-parse", "HEAD").decode().strip(), incoming)
        self.assertEqual(result["local_catalog"], "CURRENT")
        self.assert_preserved_catalog(result, originals)
        backup = Path(result["catalog_preservation"])
        self.assertEqual((backup / "original.index").read_bytes(), original_index)
        for path, content in indexed.items():
            self.assertEqual((backup / "indexed" / path).read_bytes(), content)
        self.assertEqual(receipt.read_bytes(), receipt_bytes)

    @unittest.skipUnless(sys.platform == "win32", "OS-enforced catalog transaction requires Windows")
    def test_catalog_file_transaction_blocks_foreign_write_at_the_compared_write_boundary(self):
        path = self.root / catalog.INDEX
        before = path.read_bytes()
        incoming, canary = b"incoming catalog\n", b"foreign concurrent edit must survive\n"
        actual_write, attempted = os.write, []
        def foreign_write(descriptor, content):
            if not attempted and content == incoming:
                attempted.append(True)
                with self.assertRaises(OSError):
                    path.write_bytes(canary)
            return actual_write(descriptor, content)
        with patch.object(sync.os, "write", foreign_write):
            sync.replace_catalog_file(path, before, incoming)
        self.assertEqual(attempted, [True])
        self.assertEqual(path.read_bytes(), incoming)
        # A writer already holding the file also prevents our acquisition and keeps its bytes intact.
        path.write_bytes(canary)
        with path.open("r+b"):
            with self.assertRaises(OSError):
                sync.replace_catalog_file(path, canary, incoming)
        self.assertEqual(path.read_bytes(), canary)

    @unittest.skipUnless(sys.platform == "win32", "OS-enforced catalog transaction requires Windows")
    def test_catalog_file_transaction_restores_partial_write_before_releasing_exclusivity(self):
        path = self.root / catalog.INDEX
        before, incoming = path.read_bytes(), b"a new catalog longer than one write\n"
        actual_write, calls = os.write, []
        def short_then_failed(descriptor, content):
            if not calls:
                calls.append("short")
                return actual_write(descriptor, content[:3])
            if calls == ["short"]:
                calls.append("failed")
                raise OSError("injected write failure")
            return actual_write(descriptor, content)
        with patch.object(sync.os, "write", short_then_failed), self.assertRaisesRegex(OSError, "injected write failure"):
            sync.replace_catalog_file(path, before, incoming)
        self.assertEqual(calls, ["short", "failed"])
        self.assertEqual(path.read_bytes(), before)

    def test_catalog_file_transaction_has_no_advisory_fallback_on_unsupported_platform(self):
        path = self.root / catalog.INDEX
        before = path.read_bytes()
        with patch.object(sync.sys, "platform", "unsupported"), self.assertRaisesRegex(ValueError, "unavailable on this platform"):
            sync.replace_catalog_file(path, before, b"incoming\n")
        self.assertEqual(path.read_bytes(), before)

    def test_absorbed_catalog_rejects_staged_overlap_and_authored_controls_without_mutation(self):
        preserved = self.edit_local_catalog()
        self.publish_absorbed_catalog()
        receipt, receipt_bytes = self.old_publication_receipt()
        head = self.git("rev-parse", "HEAD")
        installed = (self.installed / "SKILL.md").read_bytes()
        for path in ["assets/example/src/lib.rs", catalog.INDEX, catalog.SKILL + "/SKILL.md"]:
            with self.subTest(path=path):
                original = (self.root / path).read_bytes()
                self.write(path, "staged work absent from both base and incoming\n")
                self.git("add", path)
                (self.root / path).write_bytes(original)
                index = (self.root / ".git/index").read_bytes()
                with self.assertRaises(ValueError):
                    sync.before_use(self.root, [self.root / catalog.SKILL, self.installed], str(self.remote))
                self.assertEqual((self.root / ".git/index").read_bytes(), index)
                self.assertEqual(self.git("rev-parse", "HEAD"), head)
                self.assertEqual((self.installed / "SKILL.md").read_bytes(), installed)
                self.assertEqual(receipt.read_bytes(), receipt_bytes)
                for source, content in preserved.items():
                    self.assertEqual(source.read_bytes(), content)
                # Restore only this synthetic fixture index before the next independent case.
                self.git("read-tree", "HEAD")
        self.write("assets/example/src/lib.rs", "unfinished working content\n")
        index = (self.root / ".git/index").read_bytes()
        with self.assertRaisesRegex(ValueError, "local overlap does not match incoming"):
            sync.before_use(self.root, [self.installed], str(self.remote))
        self.assertEqual((self.root / ".git/index").read_bytes(), index)
        self.assertEqual(self.git("rev-parse", "HEAD"), head)
        self.assertEqual((self.root / "assets/example/src/lib.rs").read_text(), "unfinished working content\n")
        self.assertEqual(receipt.read_bytes(), receipt_bytes)
        for source, content in preserved.items():
            self.assertEqual(source.read_bytes(), content)

    def test_conflicting_catalog_field_stops_without_receipt_or_state_changes(self):
        self.edit_local_catalog()
        path = self.root / catalog.INDEX
        index = yaml.safe_load(path.read_text())
        index["assets"][0]["purpose"] = "local field choice"
        path.write_text(yaml.safe_dump(index, sort_keys=False), encoding="utf-8")
        catalog.refresh(self.root)
        preserved = {p: p.read_bytes() for p in [path, self.root / catalog.SKILL / "references/assets-index.yaml"]}
        receipt, receipt_bytes = self.old_publication_receipt()
        self.publish_absorbed_catalog(conflicting_field=True)
        head, index_bytes = self.git("rev-parse", "HEAD"), (self.root / ".git/index").read_bytes()
        installed = (self.installed / "references/assets-index.yaml").read_bytes()
        with self.assertRaisesRegex(ValueError, "incoming catalog update conflicts"):
            sync.before_use(self.root, [self.installed], str(self.remote))
        self.assertEqual(self.git("rev-parse", "HEAD"), head)
        self.assertEqual((self.root / ".git/index").read_bytes(), index_bytes)
        self.assertEqual((self.installed / "references/assets-index.yaml").read_bytes(), installed)
        self.assertEqual(receipt.read_bytes(), receipt_bytes)
        for path, content in preserved.items():
            self.assertEqual(path.read_bytes(), content)

    def test_absorbed_catalog_does_not_replace_installed_control_customization(self):
        preserved = self.edit_local_catalog()
        self.publish_absorbed_catalog()
        control = self.installed / "SKILL.md"
        control.write_bytes(b"device-local authored customization\n")
        head, index = self.git("rev-parse", "HEAD"), (self.root / ".git/index").read_bytes()
        receipt, receipt_bytes = self.old_publication_receipt()
        with self.assertRaisesRegex(ValueError, "local skill edits preserved"):
            sync.before_use(self.root, [self.installed], str(self.remote))
        self.assertEqual(control.read_bytes(), b"device-local authored customization\n")
        self.assertEqual(self.git("rev-parse", "HEAD"), head)
        self.assertEqual((self.root / ".git/index").read_bytes(), index)
        self.assertEqual(receipt.read_bytes(), receipt_bytes)
        for path, content in preserved.items():
            self.assertEqual(path.read_bytes(), content)

    @unittest.skipUnless(sys.platform == "win32", "OS-enforced catalog transaction requires Windows")
    def test_absorbed_catalog_rolls_back_pair_when_normal_git_rejects_a_late_overlap(self):
        preserved = self.edit_local_catalog()
        self.publish_absorbed_catalog(incoming_unrelated=True)
        receipt, receipt_bytes = self.old_publication_receipt()
        head, index = self.git("rev-parse", "HEAD"), (self.root / ".git/index").read_bytes()
        installed = (self.installed / "references/assets-index.yaml").read_bytes()
        source = (self.root / "assets/example/src/lib.rs").read_bytes()
        actual_run, injected = sync.run, []
        def concurrent_edit(root, *args, env=None, data=None):
            if Path(root) == self.root and args[:3] == ("merge", "--ff-only", "--no-edit") and env:
                injected.append(True)
                self.assertEqual((self.root / catalog.INDEX).read_bytes(), actual_run(root, "show", "origin/main:assets-index.yaml"))
                self.write("unrelated.txt", "late foreign working edit\n")
            return actual_run(root, *args, env=env, data=data)
        with patch.object(sync, "run", concurrent_edit), self.assertRaises(ValueError):
            sync.before_use(self.root, [self.installed], str(self.remote))
        self.assertEqual(injected, [True])
        self.assertEqual(self.git("rev-parse", "HEAD"), head)
        self.assertEqual((self.root / ".git/index").read_bytes(), index)
        self.assertEqual((self.root / "unrelated.txt").read_text(), "late foreign working edit\n")
        self.assertEqual((self.installed / "references/assets-index.yaml").read_bytes(), installed)
        self.assertEqual((self.root / "assets/example/src/lib.rs").read_bytes(), source)
        self.assertFalse((self.root / "assets/rss").exists())
        self.assertFalse((self.root / ".git/index.lock").exists())
        self.assertEqual(receipt.read_bytes(), receipt_bytes)
        for path, content in preserved.items():
            self.assertEqual(path.read_bytes(), content)
        backups = list((self.root / ".local/asset-bootstrap").glob("adopt-*/preservation.json"))
        self.assertEqual(len(backups), 1)
        for path, content in preserved.items():
            self.assertEqual((backups[0].parent / path.relative_to(self.root)).read_bytes(), content)

    @unittest.skipUnless(sys.platform == "win32", "OS-enforced catalog transaction requires Windows")
    def test_absorbed_catalog_merge_stays_pinned_if_fetched_ref_moves_after_check(self):
        self.edit_local_catalog()
        incoming = self.publish_absorbed_catalog()
        sync.origin(self.root, str(self.remote))
        tree = self.git("rev-parse", incoming + "^{tree}").decode().strip()
        later = sync.run(self.root, "commit-tree", tree, "-p", incoming, data=b"later incoming candidate\n").decode().strip()
        actual_run, injected = sync.run, []
        def move_ref(root, *args, env=None, data=None):
            if Path(root) == self.root and args[:3] == ("merge", "--ff-only", "--no-edit") and env:
                injected.append(True)
                actual_run(root, "update-ref", "refs/remotes/origin/main", later)
            return actual_run(root, *args, env=env, data=data)
        with patch.object(sync, "run", move_ref):
            result = sync.before_use(self.root, [self.installed], str(self.remote))
        self.assertEqual(injected, [True])
        self.assertEqual(result["origin_revision"], incoming)
        self.assertEqual(self.git("rev-parse", "HEAD").decode().strip(), incoming)
        self.assertEqual(self.git("rev-parse", "origin/main").decode().strip(), later)

    @unittest.skipUnless(sys.platform == "win32", "OS-enforced catalog transaction requires Windows")
    def test_absorbed_catalog_index_promotion_failure_retains_lock_and_advanced_recovery_state(self):
        originals = self.edit_local_catalog()
        incoming = self.publish_absorbed_catalog()
        original_index = (self.root / ".git/index").read_bytes()
        receipt, receipt_bytes = self.old_publication_receipt()
        actual_replace, failed = os.replace, []
        def fail_index_promotion(source, destination):
            if Path(destination) == self.root / ".git/index":
                failed.append(True)
                raise OSError("injected index promotion failure")
            return actual_replace(source, destination)
        with patch.object(sync.os, "replace", fail_index_promotion), self.assertRaises(sync.AdoptionRecoveryRequired) as stopped:
            sync.before_use(self.root, [self.installed], str(self.remote))
        self.assertEqual(failed, [True])
        recovery = stopped.exception.recovery
        self.assertEqual(recovery["phase"], "INDEX_PROMOTION_REQUIRED")
        self.assertEqual(recovery["observed_head"], incoming)
        self.assertEqual(self.git("rev-parse", "HEAD").decode().strip(), incoming)
        self.assertEqual((self.root / ".git/index").read_bytes(), original_index)
        candidate = Path(recovery["candidate_index"])
        self.assertTrue(candidate.is_file())
        self.assertEqual(sync.hashlib.sha256(candidate.read_bytes()).hexdigest(), recovery["candidate_index_sha256"])
        lock = self.root / ".git/index.lock"
        self.assertTrue(lock.is_file())
        self.assertEqual(json.loads(lock.read_text())["journal"], recovery["journal"])
        self.assertEqual(json.loads(Path(recovery["journal"]).read_text())["observed_head"], incoming)
        self.assertEqual(receipt.read_bytes(), receipt_bytes)
        self.assert_preserved_catalog({"catalog_preservation": str(candidate.parent)}, originals)

    @unittest.skipUnless(sys.platform == "win32", "OS-enforced catalog transaction requires Windows")
    def test_absorbed_catalog_late_unpaired_edit_stops_before_installed_copy_success(self):
        self.edit_local_catalog()
        incoming = self.publish_absorbed_catalog()
        installed = (self.installed / "references/assets-index.yaml").read_bytes()
        actual_advance = sync.advance_checkout
        canary = b"foreign edit after adoption guards close\n"
        def late_edit(*args, **kwargs):
            result = actual_advance(*args, **kwargs)
            (self.root / catalog.INDEX).write_bytes(canary)
            return result
        with patch.object(sync, "advance_checkout", late_edit), self.assertRaisesRegex(ValueError, "inventory mismatch after adoption"):
            sync.before_use(self.root, [self.installed], str(self.remote))
        self.assertEqual(self.git("rev-parse", "HEAD").decode().strip(), incoming)
        self.assertEqual((self.root / catalog.INDEX).read_bytes(), canary)
        self.assertEqual((self.installed / "references/assets-index.yaml").read_bytes(), installed)
        self.assertFalse((self.root / ".git/index.lock").exists())

    @unittest.skipUnless(sys.platform == "win32", "OS-enforced catalog transaction requires Windows")
    def test_absorbed_catalog_late_matching_pair_is_preserved_as_unpublished(self):
        self.edit_local_catalog()
        self.publish_absorbed_catalog()
        actual_advance, foreign = sync.advance_checkout, {}
        def late_pair(*args, **kwargs):
            result = actual_advance(*args, **kwargs)
            index = yaml.safe_load((self.root / catalog.INDEX).read_text())
            index["assets"][0]["capabilities"].append("late unpublished edit")
            content = yaml.safe_dump(index, sort_keys=False).encode("utf-8")
            for path in [catalog.INDEX, catalog.SKILL + "/references/assets-index.yaml"]:
                (self.root / path).write_bytes(content)
                foreign[path] = content
            return result
        with patch.object(sync, "advance_checkout", late_pair):
            result = sync.before_use(self.root, [self.installed], str(self.remote))
        self.assertEqual(result["local_catalog"], "UNPUBLISHED")
        self.assertEqual(result["source_synchronization"], "NOT_SYNCHRONIZED")
        for path, content in foreign.items():
            self.assertEqual((self.root / path).read_bytes(), content)
        self.assertNotIn(b"late unpublished edit", (self.installed / "references/assets-index.yaml").read_bytes())

    def test_divergent_commits_stop_without_automatic_merge(self):
        self.write("local.txt", "local commit")
        self.git("add", "local.txt")
        self.git("commit", "-m", "local-only commit")
        head = self.git("rev-parse", "HEAD")
        self.update_other_device("remote.txt", "remote commit")
        with self.assertRaisesRegex(ValueError, "diverge"):
            sync.before_use(self.root, [self.installed], str(self.remote))
        self.assertEqual(self.git("rev-parse", "HEAD"), head)

    def test_publishes_only_scoped_validated_candidate_and_preserves_real_index(self):
        self.write("assets/example/src/lib.rs", "pub fn answer() -> u8 { 43 }\n")
        self.write("unrelated.txt", "private staged work\n")
        self.git("add", "unrelated.txt")
        before = self.git("ls-files", "-s")
        result = sync.publish(self.root, self.plan(), "validated update", str(self.remote))
        self.assertEqual(result["status"], "PUBLISHED")
        self.assertEqual(before, self.git("ls-files", "-s"))
        self.assertEqual(self.git("show", result["revision"] + ":unrelated.txt"), b"original\n")
        self.assertIn(b"43", self.git("show", result["revision"] + ":assets/example/src/lib.rs"))

    def test_publishes_explicit_executable_paths_without_mutating_real_index(self):
        self.write("assets/example/src/lib.rs", "pub fn answer() -> u8 { 43 }\n")
        self.write("assets/example/bin/run.sh", "#!/bin/sh\nexit 0\n")
        plan = self.plan()
        plan["assets"][0]["paths"].append("assets/example/bin/run.sh")
        plan["assets"][0]["executable_paths"] = ["assets/example/bin/run.sh"]
        before = self.git("ls-files", "-s")
        result = sync.publish(self.root, plan, "validated executable asset", str(self.remote))
        self.assertEqual(before, self.git("ls-files", "-s"))
        row = self.git("ls-tree", result["revision"], "assets/example/bin/run.sh").decode()
        self.assertTrue(row.startswith("100755 blob "))

    def test_failed_proof_does_not_push(self):
        before = sync.origin(self.root, str(self.remote))[2]
        self.write("assets/example/src/lib.rs", "pub fn answer() -> u8 { 43 }\n")
        with self.assertRaisesRegex(ValueError, "asset proof failed"):
            sync.publish(self.root, self.plan("raise SystemExit(9)"), "bad update", str(self.remote))
        self.assertEqual(sync.origin(self.root, str(self.remote))[2], before)

    def test_proof_runs_against_published_candidate_without_unrelated_local_inputs(self):
        self.write("assets/example/src/lib.rs", "pub fn answer() -> u8 { 43 }\n")
        self.write("unrelated.txt", "local-only dependency\n")
        code = "from pathlib import Path; assert Path('../../unrelated.txt').read_text() == 'original\\n'"
        result = sync.publish(self.root, self.plan(code), "isolated proof", str(self.remote))
        self.assertEqual(result["status"], "PUBLISHED")

    def test_remote_advance_during_proof_stops_without_overwriting_other_device(self):
        self.write("assets/example/src/lib.rs", "pub fn answer() -> u8 { 43 }\n")
        original = sync.origin
        calls = 0
        def advance(root, expected):
            nonlocal calls
            calls += 1
            if calls == 2:
                self.update_other_device("winner.txt", "other device wins\n")
            return original(root, expected)
        with patch.object(sync, "origin", side_effect=advance):
            with self.assertRaisesRegex(ValueError, "origin advanced during proof"):
                sync.publish(self.root, self.plan(), "concurrent update", str(self.remote))
        self.assertEqual(self.git("show", "origin/main:winner.txt"), b"other device wins\n")
        self.assertIn(b"42", self.git("show", "origin/main:assets/example/src/lib.rs"))

    def test_next_before_use_reconciles_own_publication_and_preserves_other_staged_work(self):
        self.write("assets/example/src/lib.rs", "pub fn answer() -> u8 { 43 }\n")
        self.write("unrelated.txt", "private staged work\n")
        self.git("add", "unrelated.txt")
        result = sync.publish(self.root, self.plan(), "validated update", str(self.remote))
        sync.before_use(self.root, [self.installed], str(self.remote))
        self.assertEqual(self.git("rev-parse", "HEAD").decode().strip(), result["revision"])
        self.assertEqual(self.git("diff", "--cached", "--name-only").decode().strip(), "unrelated.txt")

    def test_new_edits_after_publication_are_preserved_and_stop_adoption(self):
        self.write("assets/example/src/lib.rs", "pub fn answer() -> u8 { 43 }\n")
        sync.publish(self.root, self.plan(), "validated update", str(self.remote))
        self.write("assets/example/src/lib.rs", "pub fn answer() -> u8 { 44 }\n")
        before = self.git("ls-files", "-s")
        head = self.git("rev-parse", "HEAD")
        with self.assertRaisesRegex(ValueError, "new local edits conflict"):
            sync.before_use(self.root, [self.installed], str(self.remote))
        self.assertEqual(before, self.git("ls-files", "-s"))
        self.assertEqual(head, self.git("rev-parse", "HEAD"))
        self.assertIn("44", (self.root / "assets/example/src/lib.rs").read_text())

    def test_publication_from_a_separate_git_worktree(self):
        worktree = self.base / "asset-worktree"
        self.git("worktree", "add", "--detach", str(worktree), "HEAD")
        (worktree / "assets/example/src/lib.rs").write_text("pub fn answer() -> u8 { 43 }\n")
        result = sync.publish(worktree, self.plan(), "worktree update", str(self.remote))
        self.assertEqual(result["status"], "PUBLISHED")
        self.assertIn("42", (self.root / "assets/example/src/lib.rs").read_text())

    def test_restored_older_official_skill_updates_after_checkout_was_already_pulled(self):
        (self.installed / sync.STATE).unlink()
        self.update_other_device(catalog.SKILL + "/SKILL.md", "---\nname: create-assets\ndescription: Test\n---\nnew official rules\n")
        self.git("fetch", "origin")
        self.git("merge", "--ff-only", "origin/main")
        result = sync.before_use(self.root, [self.installed], str(self.remote))
        self.assertEqual(result["status"], "UPDATED")
        self.assertIn("new official rules", (self.installed / "SKILL.md").read_text())

    def test_unrelated_edits_inside_selected_asset_are_preserved_and_not_published(self):
        self.write("assets/example/src/lib.rs", "pub fn answer() -> u8 { 43 }\n")
        self.write("assets/example/README.md", "unfinished unrelated manual edit\n")
        self.git("add", "assets/example/README.md")
        before = self.git("ls-files", "-s")
        result = sync.publish(self.root, self.plan(), "scoped files", str(self.remote))
        self.assertEqual(before, self.git("ls-files", "-s"))
        self.assertEqual(self.git("show", result["revision"] + ":assets/example/README.md"), b"The answer must be 42.\n")

    def test_glob_path_does_not_sweep_unrelated_files_into_publication(self):
        self.write("assets/example/src/lib.rs", "pub fn answer() -> u8 { 43 }\n")
        self.write("assets/example/src/unrelated.rs", "unfinished unrelated code\n")
        plan = self.plan()
        plan["assets"][0]["paths"] = ["assets/example/src/*.rs"]
        before = self.git("ls-files", "-s")
        revision = sync.origin(self.root, str(self.remote))[2]
        with self.assertRaisesRegex(ValueError, "exact existing or tracked deleted file"):
            sync.publish(self.root, plan, "invalid wildcard", str(self.remote))
        self.assertEqual(before, self.git("ls-files", "-s"))
        self.assertEqual(sync.origin(self.root, str(self.remote))[2], revision)


if __name__ == "__main__":
    unittest.main()
