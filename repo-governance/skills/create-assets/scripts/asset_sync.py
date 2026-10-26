"""Before-use updates and scoped, independently validated asset publication.

Python 3.11+, Git. No shell execution, stash, reset, force-push, or branch deletion.
"""
import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from contextlib import ExitStack
from pathlib import Path, PurePosixPath

REPOSITORY = "https://github.com/Nuntissura/assets"
SKILL = "repo-governance/skills/create-assets"
STATE = ".asset-installation.json"


def run(root, *args, env=None, data=None):
    result = subprocess.run(["git", "-c", "diff.autoRefreshIndex=false", "-C", str(root), "--literal-pathspecs", *args], input=data, capture_output=True, env=env)
    if result.returncode:
        raise ValueError(result.stderr.decode("utf-8", "replace").strip())
    return result.stdout


def identity(value):
    return value.strip().replace("git@github.com:", "https://github.com/").removesuffix(".git").rstrip("/")


def safe_path(value):
    p = PurePosixPath(value)
    if not value or p.is_absolute() or any(x in p.parts for x in ("..", ".git")) or ":" in value or "\\" in value:
        raise ValueError(f"unsafe path: {value}")
    return p


def sha(data):
    return hashlib.sha256(data.replace(b"\r\n", b"\n")).hexdigest()


def origin(root, expected=REPOSITORY):
    actual = run(root, "remote", "get-url", "origin").decode().strip()
    if identity(actual) != identity(expected):
        raise ValueError("origin identity mismatch; no synchronization performed")
    if run(root, "ls-files", "-u").strip():
        raise ValueError("unresolved Git conflicts; no synchronization performed")
    for name in ("MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD", "rebase-merge", "rebase-apply"):
        value = run(root, "rev-parse", "--git-path", name).decode().strip()
        path = Path(value) if Path(value).is_absolute() else root / value
        if path.exists():
            raise ValueError(f"Git operation in progress: {name}")
    run(root, "fetch", "--prune", "origin", "+refs/heads/*:refs/remotes/origin/*")
    lines = run(root, "ls-remote", "--symref", "origin", "HEAD").decode().splitlines()
    branch = next((line.split()[1].removeprefix("refs/heads/") for line in lines if line.startswith("ref: ")), None)
    if not branch:
        raise ValueError("origin did not advertise its default branch")
    ref = "refs/remotes/origin/" + branch
    revision = run(root, "rev-parse", ref).decode().strip()
    return branch, ref, revision


def skill_snapshot(root, revision):
    names = run(root, "ls-tree", "-r", "--name-only", revision, "--", SKILL).decode().splitlines()
    result = {}
    for name in names:
        relative = name.removeprefix(SKILL + "/")
        safe_path(relative)
        result[relative] = run(root, "show", revision + ":" + name)
    if "SKILL.md" not in result or "references/assets-index.yaml" not in result:
        raise ValueError("origin has no complete create-assets skill and inventory")
    if result["references/assets-index.yaml"].replace(b"\r\n", b"\n") != run(root, "show", revision + ":assets-index.yaml").replace(b"\r\n", b"\n"):
        raise ValueError("origin skill inventory does not match its canonical catalog")
    return result


def installed_state(root, destination, incoming):
    state_file = destination / STATE
    existing = {p.relative_to(destination).as_posix(): p for p in destination.rglob("*")
                if p.is_file() and "__pycache__" not in p.parts and p.name != STATE} if destination.exists() else {}
    if state_file.is_file():
        state = json.loads(state_file.read_text(encoding="utf-8"))
    else:
        observed = {name: sha(path.read_bytes()) for name, path in existing.items()}
        current = {name: sha(data) for name, data in incoming.items()}
        if not observed or observed == current:
            state = {"files": observed}
        else:
            # A restored mirror may contain an older official skill but no device-local installation state.
            history = run(root, "log", "--all", "--format=%H", "--", SKILL).decode().splitlines()
            state = None
            for revision in history:
                try:
                    snapshot = skill_snapshot(root, revision)
                except ValueError:
                    continue
                candidate = {name: sha(data) for name, data in snapshot.items()}
                if candidate == observed:
                    state = {"files": candidate}
                    break
            if state is None:
                raise ValueError("local skill edits preserved; installed files match no recorded repository version")
    baseline, pending = state["files"], state.get("pending", {})
    for name in set(baseline) | set(pending):
        safe_path(name)
    conflicts = []
    for name, path in existing.items():
        allowed = {baseline.get(name), pending.get(name)} - {None}
        if sha(path.read_bytes()) not in allowed:
            conflicts.append(name)
    for name in baseline:
        if name not in existing and name not in pending:
            conflicts.append(name + " (locally deleted)")
    if conflicts:
        raise ValueError("local skill edits preserved; conflicts: " + ", ".join(sorted(conflicts)))
    return state


def install_snapshot(destination, incoming, state, revision):
    destination.mkdir(parents=True, exist_ok=True)
    pending = {name: sha(data) for name, data in incoming.items()}
    state_file = destination / STATE
    state_file.write_text(json.dumps({"files": state["files"], "pending": pending,
                                     "revision": revision}, indent=2), encoding="utf-8")
    for name, content in incoming.items():
        target = destination / safe_path(name)
        if not target.resolve().is_relative_to(destination.resolve()):
            raise ValueError(f"skill path escapes install directory: {name}")
        target.parent.mkdir(parents=True, exist_ok=True)
        temporary = target.with_name(target.name + ".asset-update")
        temporary.write_bytes(content)
        os.replace(temporary, target)
    for name in set(state["files"]) - set(incoming):
        path = destination / safe_path(name)
        if path.exists():
            path.unlink()
    state_file.write_text(json.dumps({"files": pending, "revision": revision}, indent=2), encoding="utf-8")


def catalog_is_absorbed(baseline, local, incoming):
    # Match the existing catalog hash's newline normalization. Never resolve a conflicting field.
    with tempfile.TemporaryDirectory(prefix="asset-catalog-proof-") as directory:
        paths = [Path(directory) / name for name in ("local", "base", "incoming")]
        for path, content in zip(paths, (local, baseline, incoming)):
            path.write_bytes(content.replace(b"\r\n", b"\n"))
        merged = subprocess.run(["git", "merge-file", "-p", "--", *map(str, paths)], capture_output=True)
        return merged.returncode == 0 and merged.stdout == incoming.replace(b"\r\n", b"\n")


class AdoptionRecoveryRequired(ValueError):
    def __init__(self, recovery):
        self.recovery = recovery
        super().__init__(f"asset adoption requires recovery: {recovery['phase']}; "
                         f"HEAD={recovery.get('observed_head', 'unknown')}; journal={recovery.get('journal')}")


def catalog_file(path, writable):
    if sys.platform != "win32":
        raise ValueError("exclusive catalog adoption is unavailable on this platform; all work preserved")
    import ctypes
    from ctypes import wintypes
    import msvcrt
    api = ctypes.WinDLL("kernel32", use_last_error=True)
    api.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p,
                               wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
    api.CreateFileW.restype = wintypes.HANDLE
    api.CloseHandle.argtypes, api.CloseHandle.restype = [wintypes.HANDLE], wintypes.BOOL
    api.GetFileInformationByHandleEx.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
    api.GetFileInformationByHandleEx.restype = wintypes.BOOL
    name = str(path.absolute())
    if not name.startswith("\\\\?\\"):
        name = "\\\\?\\UNC\\" + name[2:] if name.startswith("\\\\") else "\\\\?\\" + name
    # Readers permit only readers. A writer permits no other read/write/delete opens.
    handle = api.CreateFileW(name, 0xC0000000 if writable else 0x80000000,
                             0 if writable else 1, None, 3, 0x00200000, None)
    if handle == ctypes.c_void_p(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    descriptor = None
    try:
        attributes = (wintypes.DWORD * 2)()
        if not api.GetFileInformationByHandleEx(handle, 9, ctypes.byref(attributes), ctypes.sizeof(attributes)):
            raise ctypes.WinError(ctypes.get_last_error())
        if attributes[0] & (0x10 | 0x400):
            raise ValueError("catalog adoption requires an ordinary file")
        descriptor = msvcrt.open_osfhandle(handle, 0)
        handle = None  # The CRT descriptor now owns and closes this exact native handle.
        os.set_inheritable(descriptor, False)
        msvcrt.setmode(descriptor, os.O_BINARY)
        stream = os.fdopen(descriptor, "r+b" if writable else "rb", buffering=0)
        descriptor = None
        return stream
    except BaseException:
        if descriptor is not None:
            os.close(descriptor)
        if handle is not None:
            api.CloseHandle(handle)
        raise


def write_catalog_descriptor(descriptor, content):
    os.lseek(descriptor, 0, os.SEEK_SET)
    offset = 0
    while offset < len(content):
        count = os.write(descriptor, content[offset:])
        if count <= 0:
            raise OSError("catalog write made no progress")
        offset += count
    os.ftruncate(descriptor, len(content))
    os.fsync(descriptor)


def replace_catalog_file(path, expected, content):
    # Comparison and write use one OS-exclusive handle; never rename over a separately read path.
    with catalog_file(path, True) as stream:
        current = stream.read()
        if current == content:
            return False
        if current != expected:
            raise ValueError(f"catalog changed during adoption; original preserved: {path}")
        try:
            write_catalog_descriptor(stream.fileno(), content)
        except BaseException:
            try:
                write_catalog_descriptor(stream.fileno(), expected)
            except BaseException as error:
                raise AdoptionRecoveryRequired({"phase": "CATALOG_WRITE_RECOVERY_REQUIRED",
                                                 "catalog_path": str(path)}) from error
            raise
        return True


def write_adoption_journal(path, state):
    descriptor, name = tempfile.mkstemp(prefix="journal-", dir=path.parent)
    temporary = Path(name)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(json.dumps(state, indent=2).encode("utf-8"))
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        if temporary.exists():
            temporary.unlink()


def advance_checkout(root, ref, head, revision, absorbed_catalog=None):
    receipt = root / ".local/asset-publication/latest.json"
    state = json.loads(receipt.read_text(encoding="utf-8")) if receipt.is_file() else {}
    own_publication = state.get("base") == head and state.get("revision") == revision
    if not own_publication and absorbed_catalog is None:
        run(root, "merge", "--ff-only", "--no-edit", ref)
        return
    paths = list(state["paths"]) if own_publication else []
    if absorbed_catalog is not None:
        incoming_paths = set(filter(None, run(root, "diff", "--name-only", "-z", head, revision).decode().split("\0")))
        dirty = set(filter(None, run(root, "diff", "--name-only", "-z", head).decode().split("\0")))
        dirty.update(filter(None, run(root, "diff", "--cached", "--name-only", "-z", head).decode().split("\0")))
        untracked = set(filter(None, run(root, "ls-files", "--others", "-z").decode().split("\0")))
        paths = sorted(set(paths) | (incoming_paths & (dirty | untracked)) | set(absorbed_catalog))
    for path in paths:
        safe_path(path)
    # A private index adopts only independently verified incoming content, retaining unrelated staging.
    index_name = run(root, "rev-parse", "--git-path", "index").decode().strip()
    real_index = Path(index_name) if Path(index_name).is_absolute() else root / index_name
    lock_path = real_index.with_name(real_index.name + ".lock")
    with lock_path.open("x") as lock:
        originals = {}
        indexed_originals = {}
        written_catalog = []
        preservation = None
        keep_lock = False
        journal = None
        catalog_readers = ExitStack()
        state = {"base": head, "incoming": revision, "observed_head": head,
                 "real_index": str(real_index), "index_lock": str(lock_path),
                 "purpose": "adoption recovery; not a publication receipt"}
        try:
            if run(root, "rev-parse", "HEAD").decode().strip() != head or run(root, "rev-parse", ref).decode().strip() != revision:
                raise ValueError("checkout or fetched revision changed before adoption; all work preserved")
            area = root / ".local/asset-bootstrap"
            area.mkdir(parents=True, exist_ok=True)
            preservation = Path(tempfile.mkdtemp(prefix="adopt-", dir=area))
            temporary = preservation / "adopt.index"
            journal = preservation / "preservation.json"
            shutil.copyfile(real_index, preservation / "original.index")
            shutil.copyfile(real_index, temporary)
            state.update({"phase": "PREPARING", "journal": str(journal),
                          "original_index": str(preservation / "original.index"),
                          "candidate_index": str(temporary),
                          "original_index_sha256": hashlib.sha256(real_index.read_bytes()).hexdigest()})
            write_adoption_journal(journal, state)
            env = dict(os.environ, GIT_INDEX_FILE=str(temporary))
            staged = run(root, "diff", "--cached", "--name-only", "-z", head, "--", *paths).decode().split("\0")
            for path in filter(None, staged):
                if absorbed_catalog is not None and path in absorbed_catalog:
                    continue  # Both indexed catalog projections get their own three-way proof below.
                if run(root, "diff", "--cached", "--name-only", revision, "--", path).strip():
                    raise ValueError(f"new staged work conflicts with incoming adoption: {path}")
            if absorbed_catalog is not None:
                ordinary = [path for path in paths if path not in absorbed_catalog]
                if ordinary:
                    run(root, "add", "-A", "-f", "--", *ordinary, env=env)
                    if run(root, "diff", "--cached", "--name-only", revision, "--", *ordinary, env=env).strip():
                        raise ValueError("local overlap does not match incoming content; all work preserved")
                for path, expected in absorbed_catalog.items():
                    target = root / path
                    if target.is_symlink() or not target.resolve().is_relative_to(root.resolve()) or target.read_bytes() != expected:
                        raise ValueError(f"catalog changed during adoption; all work preserved: {path}")
                    incoming = run(root, "show", revision + ":" + path)
                    baseline = run(root, "show", head + ":" + path)
                    indexed = run(root, "show", ":" + path)
                    if not catalog_is_absorbed(baseline, expected, incoming) or not catalog_is_absorbed(baseline, indexed, incoming):
                        raise ValueError("incoming catalog no longer contains local edits; all work preserved")
                    original = preservation / path
                    original.parent.mkdir(parents=True, exist_ok=True)
                    original.write_bytes(expected)
                    indexed_backup = preservation / "indexed" / path
                    indexed_backup.parent.mkdir(parents=True, exist_ok=True)
                    indexed_backup.write_bytes(indexed)
                    indexed_originals[path] = hashlib.sha256(indexed).hexdigest()
                    originals[path] = (expected, incoming)
                state.update({"phase": "CATALOG_PREPARED",
                              "files": {path: hashlib.sha256(value[0]).hexdigest() for path, value in originals.items()},
                              "indexed_files": indexed_originals})
                write_adoption_journal(journal, state)
                for path, (expected, incoming) in originals.items():
                    if replace_catalog_file(root / path, expected, incoming):
                        written_catalog.append(path)
                for path, (_, incoming) in originals.items():
                    reader = catalog_readers.enter_context(catalog_file(root / path, False))
                    if reader.read() != incoming:
                        raise ValueError("catalog changed before read protection; all foreign edits retained")
            run(root, "add", "-A", "--", *paths, env=env)
            if run(root, "diff", "--cached", "--name-only", revision, "--", *paths, env=env).strip():
                context = "our publication" if own_publication else "incoming adoption"
                raise ValueError(f"new local edits conflict with {context}; all work preserved")
            state.update({"phase": "READY_TO_FAST_FORWARD",
                          "candidate_index_sha256": hashlib.sha256(temporary.read_bytes()).hexdigest()})
            write_adoption_journal(journal, state)
            lock.write(json.dumps({"journal": str(journal), "base": head, "incoming": revision}))
            lock.flush()
            os.fsync(lock.fileno())
            run(root, "merge", "--ff-only", "--no-edit", revision, env=env)
            state.update({"phase": "INDEX_PROMOTION_REQUIRED",
                          "observed_head": run(root, "rev-parse", "HEAD").decode().strip(),
                          "candidate_index_sha256": hashlib.sha256(temporary.read_bytes()).hexdigest()})
            write_adoption_journal(journal, state)
            if state["observed_head"] != revision:
                raise ValueError("HEAD changed during fast-forward; index promotion withheld")
            os.replace(temporary, real_index)
            state.update({"phase": "COMPLETE", "real_index_sha256": hashlib.sha256(real_index.read_bytes()).hexdigest()})
            write_adoption_journal(journal, state)
            return preservation if absorbed_catalog is not None else None
        except BaseException as error:
            catalog_readers.close()
            keep_lock = True  # Release only after proving old HEAD and completing any catalog rollback.
            try:
                observed_head = run(root, "rev-parse", "HEAD").decode().strip()
            except Exception as head_error:
                observed_head = None
                state["head_read_error"] = str(head_error)
            state.update({"observed_head": observed_head, "failure": str(error)})
            if observed_head == head and not isinstance(error, AdoptionRecoveryRequired):
                unreconciled = []
                for path in written_catalog:
                    expected, incoming = originals[path]
                    try:
                        replace_catalog_file(root / path, incoming, expected)
                    except (OSError, ValueError):
                        unreconciled.append(path)
                if not unreconciled:
                    state["phase"] = "ABORTED_HEAD_UNCHANGED"
                    if journal is not None:
                        try:
                            write_adoption_journal(journal, state)
                        except OSError as journal_error:
                            state.update({"phase": "JOURNAL_RECOVERY_REQUIRED", "journal_write_error": str(journal_error)})
                            raise AdoptionRecoveryRequired(state) from error
                    keep_lock = False
                    raise
                state.update({"phase": "CATALOG_RECONCILIATION_REQUIRED", "catalog_paths": unreconciled})
            else:
                if isinstance(error, AdoptionRecoveryRequired):
                    state.update(error.recovery)
                state["phase"] = (error.recovery["phase"] if isinstance(error, AdoptionRecoveryRequired)
                                  else "INDEX_PROMOTION_REQUIRED" if observed_head == revision
                                  else "HEAD_RECONCILIATION_REQUIRED")
            for name, path in [("real_index_sha256", real_index), ("candidate_index_sha256", temporary if preservation else None)]:
                try:
                    if path is not None and path.is_file():
                        state[name] = hashlib.sha256(path.read_bytes()).hexdigest()
                except OSError as hash_error:
                    state[name + "_error"] = str(hash_error)
            state["candidate_index_present"] = bool(preservation and temporary.is_file())
            state["real_index_matches_candidate"] = bool(state.get("real_index_sha256")
                and state.get("real_index_sha256") == state.get("candidate_index_sha256"))
            if observed_head == revision and state["real_index_matches_candidate"]:
                state["phase"] = "INDEX_PROMOTED_RECOVERY_VERIFICATION_REQUIRED"
            try:
                lock.seek(0)
                lock.truncate()
                lock.write(json.dumps({key: state.get(key) for key in
                    ("journal", "phase", "base", "incoming", "observed_head", "real_index_sha256", "candidate_index_sha256")}))
                lock.flush()
                os.fsync(lock.fileno())
            except OSError as lock_error:
                state["lock_journal_error"] = str(lock_error)
            if journal is not None:
                try:
                    write_adoption_journal(journal, state)
                except OSError as journal_error:
                    state["journal_write_error"] = str(journal_error)
            raise AdoptionRecoveryRequired(state) from error
        finally:
            catalog_readers.close()
            lock.close()
            if not keep_lock:
                lock_path.unlink()


def before_use(root, destinations, expected=REPOSITORY):
    branch, ref, revision = origin(root, expected)
    incoming = skill_snapshot(root, revision)
    head = run(root, "rev-parse", "HEAD").decode().strip()
    repository_skill = (root / SKILL).resolve()
    catalog_content = (root / "assets-index.yaml").read_bytes()
    published_catalog = run(root, "show", revision + ":assets-index.yaml")
    baseline_catalog = run(root, "show", head + ":assets-index.yaml")
    staged_catalog = bool(run(root, "diff", "--cached", "--name-only", head, "--", "assets-index.yaml").strip())
    local_catalog = staged_catalog or sha(catalog_content) != sha(baseline_catalog)
    absorbed_catalog = None
    if local_catalog:
        generated = repository_skill / "references/assets-index.yaml"
        if not generated.is_file() or sha(generated.read_bytes()) != sha(catalog_content):
            raise ValueError("local generated inventory mismatch; canonical catalog and skill copy preserved")
        if sha(published_catalog) != sha(baseline_catalog):
            receipt = root / ".local/asset-publication/latest.json"
            publication = json.loads(receipt.read_text(encoding="utf-8")) if receipt.is_file() else {}
            own_publication = (publication.get("base") == head and publication.get("revision") == revision
                               and {"assets-index.yaml", SKILL + "/references/assets-index.yaml"}
                               .issubset(publication.get("paths", []))
                               and sha(catalog_content) == sha(published_catalog))
            if not own_publication:
                if not catalog_is_absorbed(baseline_catalog, catalog_content, published_catalog):
                    raise ValueError("incoming catalog update conflicts with unpublished local catalog; all work preserved")
                absorbed_catalog = {"assets-index.yaml": catalog_content,
                                    SKILL + "/references/assets-index.yaml": generated.read_bytes()}
            # advance_checkout must still verify selected paths against the incoming tree and real index.
    staged_controls = run(root, "diff", "--cached", "--name-only", "-z", head, "--", SKILL).decode().split("\0")
    for path in filter(None, staged_controls):
        if local_catalog and path == SKILL + "/references/assets-index.yaml":
            indexed_inventory = run(root, "show", ":" + path)
            indexed_catalog = run(root, "show", ":assets-index.yaml")
            if sha(indexed_inventory) not in {sha(indexed_catalog), sha(catalog_content)}:
                raise ValueError("staged generated inventory mismatch; index and working catalogs preserved")
            continue
        raise ValueError(f"staged repository skill change preserved: {path}")
    baseline_skill = skill_snapshot(root, head)
    repository_files = {path.relative_to(repository_skill).as_posix(): path for path in repository_skill.rglob("*")
                        if path.is_file() and "__pycache__" not in path.parts and path.name != STATE}
    for name in sorted(set(baseline_skill) | set(repository_files)):
        if local_catalog and name == "references/assets-index.yaml":
            continue
        local = repository_files.get(name)
        if local is None or name not in baseline_skill or sha(local.read_bytes()) != sha(baseline_skill[name]):
            raise ValueError(f"local repository skill change preserved: {name}")
    # The repository inventory is a generated source projection, not an installed skill customization.
    # Git advances its official controls; only external copies receive the fetched installation.
    states = [None if destination.resolve() == repository_skill else installed_state(root, destination, incoming)
              for destination in destinations]
    preservation = None
    if head != revision:
        result = subprocess.run(["git", "-C", str(root), "merge-base", "--is-ancestor", head, ref], capture_output=True)
        if result.returncode:
            raise ValueError(f"local commits diverge from origin; preserved HEAD={head}, origin={revision}")
        # Git itself rejects incoming changes that would overwrite staged, unstaged, or untracked work.
        preservation = advance_checkout(root, ref, head, revision, absorbed_catalog)
    if run(root, "rev-parse", "HEAD").decode().strip() != revision:
        raise ValueError("checkout did not reach the fetched revision")
    with ExitStack() as final_readers:
        current_projection = None
        if absorbed_catalog is not None:
            current_catalog = final_readers.enter_context(catalog_file(root / "assets-index.yaml", False)).read()
            current_projection = final_readers.enter_context(catalog_file(repository_skill / "references/assets-index.yaml", False)).read()
        else:
            current_catalog = (root / "assets-index.yaml").read_bytes()
        local_catalog = (sha(current_catalog) != sha(published_catalog)
                         or bool(run(root, "diff", "--cached", "--name-only", revision, "--", "assets-index.yaml").strip()))
        if local_catalog:
            if current_projection is None:
                current_projection = (repository_skill / "references/assets-index.yaml").read_bytes()
            if sha(current_projection) != sha(current_catalog):
                raise ValueError("local generated inventory mismatch after adoption; foreign edits preserved, no skill installed")
        # Control-file edits are conflicts even when no branch advancement was necessary.
        for name, content in incoming.items():
            if local_catalog and name == "references/assets-index.yaml":
                continue
            local = root / SKILL / safe_path(name)
            if not local.is_file() or sha(local.read_bytes()) != sha(content):
                raise ValueError(f"local repository skill change preserved: {name}")
        for destination, state in zip(destinations, states):
            if state is not None:
                install_snapshot(destination, incoming, state, revision)
        result = {"status": "UPDATED", "origin_revision": revision, "branch": branch,
                "reread_skill": [str(p / "SKILL.md") for p in destinations],
                "local_catalog": "UNPUBLISHED" if local_catalog else "CURRENT",
                "source_synchronization": "NOT_SYNCHRONIZED" if local_catalog else "NOT_VERIFIED",
                "local_work": "preserved; repository catalog and generated inventory remain unpublished" if local_catalog
                              else "preserved; only a fast-forward and managed skill installation were allowed"}
        if preservation is not None:
            result["catalog_preservation"] = str(preservation)
        return result


def publish(root, plan, message, expected=REPOSITORY):
    branch, ref, revision = origin(root, expected)
    if run(root, "rev-parse", "HEAD").decode().strip() != revision:
        raise ValueError("run before-use and reconcile local commits before publishing")
    # The inventory is read through the validated catalog implementation, not a second parser.
    import importlib.util
    module_path = root / "repo-governance/scripts/asset_catalog.py"
    spec = importlib.util.spec_from_file_location("publication_catalog", module_path)
    catalog = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(catalog)
    index = catalog.yaml.safe_load((root / "assets-index.yaml").read_text(encoding="utf-8"))
    assets = {a["id"]: a for a in index["assets"]}
    scopes, selected_paths, executable_paths, checks = [], [], [], []
    for item in plan["assets"]:
        asset = assets[item["id"]]
        scope = str(safe_path(asset["path"]))
        scope_parts = PurePosixPath(scope).parts
        if (not scope_parts or scope_parts[0].startswith(".")
                or scope_parts[0] in {"repo-governance", "source-downloads", "node_modules", "target"}
                or not item.get("checks") or not item.get("paths")):
            raise ValueError(f"asset scope or required proof missing: {item['id']}")
        scopes.append(scope)
        item_paths = []
        for value in item["paths"]:
            path = str(safe_path(value))
            if not path.startswith(scope + "/") or (root / path).is_dir():
                raise ValueError("publication paths must name task-owned files inside their asset scope")
            if not (root / path).is_file():
                existing = run(root, "ls-tree", "-z", "HEAD", "--", path)
                if not existing or existing.split(b"\t", 1)[0].split()[1] != b"blob":
                    raise ValueError(f"publication requires an exact existing or tracked deleted file: {path}")
            selected_paths.append(path)
            item_paths.append(path)
        for value in item.get("executable_paths", []):
            path = str(safe_path(value))
            if path not in item_paths or not (root / path).is_file():
                raise ValueError("executable paths must name selected files inside their asset scope")
            executable_paths.append(path)
        for check in item["checks"]:
            cwd = str(safe_path(check.get("cwd", scope)))
            if cwd != scope and not cwd.startswith(scope + "/"):
                raise ValueError("proof working directory escapes its asset scope")
            if not isinstance(check.get("argv"), list) or not check["argv"] or not check.get("authority"):
                raise ValueError("each proof requires argv and an inspected authority/manual reference")
            safe_path(check["authority"])
            checks.append((cwd, check))
    if not scopes:
        raise ValueError("publication requires at least one asset")
    previous = catalog.yaml.safe_load(run(root, "show", revision + ":assets-index.yaml"))
    previous_assets = {a["id"]: a for a in previous["assets"]}
    selected = {a["id"] for a in plan["assets"]}
    for asset_id in set(assets) | set(previous_assets):
        if assets.get(asset_id) != previous_assets.get(asset_id) and asset_id not in selected:
            raise ValueError(f"unrelated catalog edit preserved: {asset_id}")
    for key in set(index) | set(previous):
        if key not in ("assets", "updated_at") and index.get(key) != previous.get(key):
            raise ValueError(f"catalog control change requires separate maintenance: {key}")
    # A private index preserves the user's real index, including staged work in the selected asset.
    area = root / ".local" / "asset-publication"
    area.mkdir(parents=True, exist_ok=True)
    session = Path(tempfile.mkdtemp(prefix="candidate-", dir=area))
    env = dict(os.environ, GIT_INDEX_FILE=str(session / "index"))
    run(root, "read-tree", "HEAD", env=env)
    allowed = selected_paths + ["assets-index.yaml", SKILL + "/references/assets-index.yaml"]
    run(root, "add", "-A", "--", *allowed, env=env)
    if executable_paths:
        run(root, "update-index", "--chmod=+x", "--", *executable_paths, env=env)
    paths = run(root, "diff", "--cached", "--name-only", "-z", env=env).decode().split("\0")
    if not any(paths):
        raise ValueError("no scoped changes to publish")
    tree = run(root, "write-tree", env=env).decode().strip()
    candidate = run(root, "commit-tree", tree, "-p", revision, data=(message + "\n").encode()).decode().strip()
    checkout = session / "proof"
    subprocess.run(["git", "clone", "--shared", "--no-checkout", str(root), str(checkout)],
                   check=True, capture_output=True)
    run(checkout, "config", "core.longpaths", "true")
    run(checkout, "checkout", "--detach", candidate)
    run(checkout, "remote", "set-url", "origin", run(root, "remote", "get-url", "origin").decode().strip())
    result = subprocess.run([sys.executable, str(checkout / "repo-governance/scripts/asset_catalog.py"),
                             "check", "--repo", str(checkout)], capture_output=True, text=True)
    if result.returncode:
        raise ValueError(f"candidate catalog failed; preserved at {checkout}\n{result.stdout}\n{result.stderr}")
    for cwd, check in checks:
        authority = checkout / check["authority"]
        if not authority.is_file():
            raise ValueError(f"proof authority missing from candidate: {check['authority']}")
        result = subprocess.run(check["argv"], cwd=checkout / cwd, capture_output=True, text=True)
        if result.returncode:
            raise ValueError(f"asset proof failed; candidate preserved at {checkout}\n{result.stdout}\n{result.stderr}")
    if run(checkout, "status", "--porcelain").strip():
        raise ValueError(f"proof modified candidate source; preserved at {checkout}")
    # A concurrent remote writer makes the ordinary push reject; never retry by force or overwrite.
    _, _, current = origin(root, expected)
    if current != revision:
        raise ValueError(f"origin advanced during proof: {revision} -> {current}; candidate preserved at {checkout}")
    run(root, "push", "origin", candidate + ":refs/heads/" + branch)
    _, _, observed = origin(root, expected)
    if observed != candidate:
        raise ValueError(f"publication requires reconciliation: pushed {candidate}, observed {observed}")
    if run(root, "rev-parse", observed + "^{tree}").decode().strip() != tree:
        raise ValueError("published tree did not match the validated candidate")
    (area / "latest.json").write_text(json.dumps({"base": revision, "revision": candidate,
                                                 "paths": list(filter(None, paths))}, indent=2), encoding="utf-8")
    return {"status": "PUBLISHED", "revision": candidate, "tree": tree, "paths": list(filter(None, paths)),
            "proof_checkout": str(checkout),
            "local_work": "real index and working tree unchanged; reconcile the new origin revision before further work"}


def discover_repo(value):
    if value:
        return Path(value).resolve()
    configured = os.environ.get("ASSETS_REPO")
    if not configured:
        result = subprocess.run(["git", "config", "--global", "--get", "assets.repo"], capture_output=True, text=True)
        configured = result.stdout.strip()
    if configured:
        return Path(configured).resolve()
    result = subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True)
    if result.returncode:
        raise ValueError("no asset checkout configured; provide --repo or ASSETS_REPO")
    return Path(result.stdout.strip()).resolve()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("before-use", "publish"))
    parser.add_argument("--repo")
    parser.add_argument("--skill-dir", action="append", type=Path)
    parser.add_argument("--plan", type=Path)
    parser.add_argument("--message", default="Update validated reusable assets")
    args = parser.parse_args()
    try:
        root = discover_repo(args.repo)
        if args.command == "before-use":
            destinations = args.skill_dir or [Path(__file__).resolve().parents[1]]
            result = before_use(root, destinations)
        else:
            if not args.plan:
                raise ValueError("publish requires --plan with asset IDs and authority-derived proof commands")
            result = publish(root, json.loads(args.plan.read_text(encoding="utf-8")), args.message)
        print(json.dumps(result, indent=2))
        return 0
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as exc:
        result = {"status": "STOPPED", "reason": str(exc)}
        if isinstance(exc, AdoptionRecoveryRequired):
            result.update({"recovery_required": True, "recovery": exc.recovery})
        print(json.dumps(result, indent=2))
        return 1


if __name__ == "__main__":
    sys.exit(main())
