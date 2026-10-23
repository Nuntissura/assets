import importlib.util
import json
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
