#!/usr/bin/env python3
"""Offline TEST-FIXTURE generator for hsk-studio-interop-lrcat (run once; outputs are committed).

PROVENANCE: this script uses Python's standard `sqlite3` module ONLY to write small SYNTHETIC
"lrcat-shaped" SQLite files. The product crate never links or invokes SQLite (CX-503R). The
Lightroom table and column names here follow the module brief and a concept reference; they are
NOT copied from a Lightroom-written catalog, no real catalog or personal data is involved, and
they are UNVERIFIED against any real Lightroom version. The expected-value files are computed
from the in-memory model below, never by reading the produced databases back, so they are an
oracle independent of the Rust decoder.

Usage: python -I generate_fixtures.py --out DIR     (DIR must be under the artifact root, not C:)
Outputs: synthetic-main.lrcat, synthetic-main.expected.tsv,
         synthetic-wal.lrcat, synthetic-wal.lrcat-wal, synthetic-wal-uncommitted.lrcat-wal,
         synthetic-wal.expected.tsv, PROVENANCE.json
"""
import argparse
import hashlib
import json
import os
import shutil
import sqlite3
import sys

PAGE_SIZE = 512
N_IMAGES = 300

SCHEMA = [
    "CREATE TABLE Adobe_variablesTable (id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL, name, type, value)",
    "CREATE TABLE AgLibraryRootFolder (id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL, absolutePath UNIQUE NOT NULL DEFAULT '', name NOT NULL DEFAULT '', relativePathFromCatalog)",
    "CREATE TABLE AgLibraryFolder (id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL, parentId INTEGER, pathFromRoot NOT NULL DEFAULT '', rootFolder INTEGER NOT NULL DEFAULT 0, visibility INTEGER)",
    "CREATE TABLE AgLibraryFile (id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL, baseName NOT NULL DEFAULT '', extension NOT NULL DEFAULT '', folder INTEGER NOT NULL DEFAULT 0, idx_filename UNIQUE NOT NULL DEFAULT '', originalFilename, modTime)",
    "CREATE TABLE Adobe_images (id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL, captureTime, colorLabels NOT NULL DEFAULT '', fileFormat NOT NULL DEFAULT 'unset', fileHeight, fileWidth, masterImage, orientation, pick NOT NULL DEFAULT 0, propertiesCache, rating, rootFile INTEGER NOT NULL DEFAULT 0, touchCount NOT NULL DEFAULT 0)",
    "CREATE TABLE Adobe_imageDevelopSettings (id_local INTEGER PRIMARY KEY, image INTEGER, processVersion, text, historySettingsID)",
    "CREATE TABLE AgLibraryCollection (id_local INTEGER PRIMARY KEY, creationId NOT NULL DEFAULT '', genealogy, imageCount, name NOT NULL DEFAULT '', parent, systemOnly)",
    "CREATE TABLE AgLibraryCollectionImage (id_local INTEGER PRIMARY KEY, collection INTEGER, image INTEGER, pick, positionInCollection)",
    "CREATE TABLE AgLibraryCollectionContent (id_local INTEGER PRIMARY KEY, collection INTEGER, content, owningModule)",
    "CREATE TABLE AgLibraryKeyword (id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL, genealogy, lc_name, name, parent)",
    "CREATE TABLE AgLibraryKeywordImage (id_local INTEGER PRIMARY KEY, image INTEGER, tag INTEGER)",
    "CREATE TABLE Adobe_AdditionalMetadata (id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL, image INTEGER, xmp BLOB)",
    "CREATE TABLE AgHarvestedExifMetadata (id_local INTEGER PRIMARY KEY, image INTEGER, aperture, cameraModelRef)",
    "CREATE TABLE Adobe_libraryImageDevelopHistoryStep (id_local INTEGER PRIMARY KEY, image INTEGER, name, text)",
    "CREATE TABLE Hsk_Synthetic_Unknown (id_local INTEGER PRIMARY KEY, a, b)",
    "CREATE TABLE scratch (id INTEGER PRIMARY KEY, blob BLOB)",
]

ROOTS = {1: "D:/Photos/", 2: "E:/Archive/"}
FOLDERS = {1: (1, ""), 2: (1, "Trip-2020/"), 3: (2, "2019/Wedding/")}


def blob(seed, n):
    return bytes((seed * 7 + j) % 251 for j in range(n))


def lua(pv, enable=False, pad=0):
    lines = ["s = {", "\tAutoLateralCA = 0,", f'\tProcessVersion = "{pv}",', "\tExposure2012 = 0.5,",
             '\tWhiteBalance = "As Shot",', "\tToneCurve = { 0, 0, 255, 255, },"]
    if enable:
        lines.append("\tEnableToneCurve = true,")
    for k in range(pad):
        lines.append(f"\tPad{k:04d} = {k},")
    lines.append("}")
    return "\n".join(lines) + "\n"


def make_model():
    images = []
    for i in range(1, N_IMAGES + 1):
        images.append({
            "id": i,
            "global": f"IMG-GLOBAL-{i:04d}",
            "folder": (i % 3) + 1,
            "name": f"IMG_{i:04d}.CR2",
            "base": f"IMG_{i:04d}",
            "ext": "CR2",
            "rating": None if i % 7 == 0 else i % 6,
            "pick": 1 if i % 50 == 0 else (-1 if i % 97 == 0 else 0),
            "label": "Red" if i % 10 == 0 else "",
            "capture": f"2020-05-{(i % 28) + 1:02d}T12:{i % 60:02d}:00",
            "orient": "BC" if i % 11 == 0 else "AB",
            "format": "RAW" if i % 2 else "JPG",
            "w": 6000, "h": 4000,
        })
    develop = []  # (image, text)
    develop.append((1, lua("11.0", enable=True)))
    develop.append((2, lua("99.9")))
    develop.append((3, lua("11.0", pad=300)))
    for i in range(4, 41):
        develop.append((i, lua("11.0")))
    develop.append((99999, lua("11.0")))  # orphan: no such image
    return images, develop


def insert_all(conn, images, develop):
    c = conn.cursor()
    c.executemany("INSERT INTO AgLibraryRootFolder(id_local,id_global,absolutePath,name) VALUES(?,?,?,?)",
                  [(k, f"ROOT-{k}", v, f"root{k}") for k, v in ROOTS.items()])
    c.executemany("INSERT INTO AgLibraryFolder(id_local,id_global,pathFromRoot,rootFolder) VALUES(?,?,?,?)",
                  [(k, f"FOLDER-{k}", rel, root) for k, (root, rel) in FOLDERS.items()])
    for im in images:
        insert_image(c, im)
    c.executemany("INSERT INTO Adobe_imageDevelopSettings(id_local,image,processVersion,text) VALUES(?,?,?,?)",
                  [(n + 1, img, None, text) for n, (img, text) in enumerate(develop)])
    c.executemany("INSERT INTO AgLibraryCollection(id_local,creationId,name,parent) VALUES(?,?,?,?)", [
        (1, "com.adobe.ag.library.collection", "Favorites", None),
        (2, "com.adobe.ag.library.smart_collection", "Five Stars", None),
        (3, "com.adobe.ag.library.group", "Sets", None)])
    c.executemany("INSERT INTO AgLibraryCollectionImage(id_local,collection,image) VALUES(?,?,?)",
                  [(1, 1, 1), (2, 1, 2), (3, 1, 3), (4, 1, 99999)])
    c.execute("INSERT INTO AgLibraryCollectionContent(id_local,collection,content,owningModule) VALUES(1,2,'s = { rule = 1 }','ag.library.smart_collection')")
    c.executemany("INSERT INTO AgLibraryKeyword(id_local,id_global,lc_name,name,parent) VALUES(?,?,?,?,?)",
                  [(1, "KW-1", "travel", "Travel", None), (2, "KW-2", "italy", "Italy", 1)])
    c.executemany("INSERT INTO AgLibraryKeywordImage(id_local,image,tag) VALUES(?,?,?)",
                  [(1, 1, 1), (2, 1, 2), (3, 2, 2), (4, 999, 1)])
    c.executemany("INSERT INTO Adobe_AdditionalMetadata(id_local,id_global,image,xmp) VALUES(?,?,?,?)",
                  [(i, f"META-{i}", i, blob(i, 64)) for i in range(1, 51)])
    c.executemany("INSERT INTO AgHarvestedExifMetadata(id_local,image,aperture,cameraModelRef) VALUES(?,?,?,?)",
                  [(i, i, 2.8, 1) for i in range(1, 21)])
    c.executemany("INSERT INTO Adobe_libraryImageDevelopHistoryStep(id_local,image,name,text) VALUES(?,?,?,?)",
                  [(i, 1, f"step{i}", "s = {}") for i in range(1, 6)])
    c.executemany("INSERT INTO Hsk_Synthetic_Unknown(id_local,a,b) VALUES(?,?,?)", [(i, i, f"b{i}") for i in range(1, 4)])
    c.executemany("INSERT INTO Adobe_variablesTable(id_local,id_global,name,type,value) VALUES(?,?,?,?,?)",
                  [(1, "VAR-1", "Adobe_DBVersion", "string", "0600000"), (2, "VAR-2", "Adobe_storeProviderID", "string", "x"),
                   (3, "VAR-3", "Adobe_entityCache", "string", "y")])
    c.executemany("INSERT INTO scratch(id,blob) VALUES(?,?)", [(i, blob(i, 400)) for i in range(1, 21)])


def insert_image(c, im):
    c.execute("INSERT INTO AgLibraryFile(id_local,id_global,baseName,extension,folder,idx_filename,originalFilename) VALUES(?,?,?,?,?,?,?)",
              (im["id"], f"FILE-{im['id']}", im["base"], im["ext"], im["folder"], im["name"], im["name"]))
    c.execute("INSERT INTO Adobe_images(id_local,id_global,captureTime,colorLabels,fileFormat,fileHeight,fileWidth,orientation,pick,propertiesCache,rating,rootFile) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
              (im["id"], im["global"], im["capture"], im["label"], im["format"], im["h"], im["w"], im["orient"],
               im["pick"], blob(im["id"], 24), im["rating"], im["id"]))


def build_main(path, images, develop):
    if os.path.exists(path):
        os.remove(path)
    conn = sqlite3.connect(path, isolation_level=None)
    conn.execute(f"PRAGMA page_size={PAGE_SIZE}")
    conn.execute("PRAGMA auto_vacuum=0")
    conn.execute("PRAGMA synchronous=OFF")
    conn.execute("BEGIN")
    for ddl in SCHEMA:
        conn.execute(ddl)
    insert_all(conn, images, develop)
    conn.execute("COMMIT")
    conn.execute("DROP TABLE scratch")  # leaves freelist pages in the file
    conn.close()


def tsv_dump(path, images, develop):
    def f(v):
        return "\\N" if v is None else str(v)
    with open(path, "w", newline="\n") as out:
        for im in images:
            root, rel = FOLDERS[im["folder"]]
            out.write("\t".join(["photo", f(im["id"]), f(im["global"]), ROOTS[root], rel, im["name"], f(im["rating"]),
                                 f(im["pick"]), f(im["label"]), im["capture"], im["orient"], im["format"],
                                 str(im["w"]), str(im["h"])]) + "\n")
        ids = {im["id"] for im in images}
        for img, text in develop:
            if img in ids:
                raw = text.encode()
                pv = [l for l in text.split("\n") if "ProcessVersion" in l][0].split('"')[1]
                out.write("\t".join(["develop", str(img), pv, str(len(raw)), hashlib.sha256(raw).hexdigest()]) + "\n")
        out.write("count\tdevelop_seen\t%d\ncount\tdevelop_mapped\t%d\n" % (len(develop), sum(1 for i, _ in develop if i in ids)))
        out.write("collection\t1\tFavorites\tstandard\ncollection\t2\tFive Stars\tsmart\ncollection\t3\tSets\tother\n")
        out.write("member\t1\t1\nmember\t1\t2\nmember\t1\t3\ncount\tmembers_seen\t4\ncount\tmembers_mapped\t3\n")
        out.write("keyword\t1\tTravel\t\\N\nkeyword\t2\tItaly\t1\n")
        out.write("assign\t1\t1\nassign\t1\t2\nassign\t2\t2\ncount\tassign_seen\t4\ncount\tassign_mapped\t3\n")
        out.write("count\tphotos\t%d\nschema_version\t0600000\n" % len(images))


def sha(path):
    with open(path, "rb") as fh:
        return hashlib.sha256(fh.read()).hexdigest()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    out = os.path.abspath(ap.parse_args().out)
    if out.upper().startswith("C:"):
        sys.exit("refusing to write under C:")
    os.makedirs(out, exist_ok=True)
    work = os.path.join(out, "work")
    os.makedirs(work, exist_ok=True)

    images, develop = make_model()
    main_path = os.path.join(out, "synthetic-main.lrcat")
    build_main(main_path, images, develop)
    tsv_dump(os.path.join(out, "synthetic-main.expected.tsv"), images, develop)

    # WAL variant: same base, then committed changes that stay in the -wal (never checkpointed).
    wal_work = os.path.join(work, "wal-work.lrcat")
    build_main(wal_work, images, develop)
    conn = sqlite3.connect(wal_work, isolation_level=None)
    assert conn.execute("PRAGMA journal_mode=WAL").fetchone()[0] == "wal"
    conn.execute("PRAGMA wal_autocheckpoint=0")
    conn.execute("PRAGMA synchronous=OFF")
    conn.execute("BEGIN")
    images2 = [dict(im) for im in images]
    images2[0].update(rating=5, pick=1)  # image 1: rating 1 -> 5, pick 0 -> 1
    conn.execute("UPDATE Adobe_images SET rating=5, pick=1 WHERE id_local=1")
    images2[1].update(capture="2021-01-01T00:00:00")
    conn.execute("UPDATE Adobe_images SET captureTime='2021-01-01T00:00:00' WHERE id_local=2")
    new = dict(images[0], id=N_IMAGES + 1, global_="x")
    new.update({"id": N_IMAGES + 1, "global": f"IMG-GLOBAL-{N_IMAGES + 1:04d}", "folder": 2,
                "name": f"IMG_{N_IMAGES + 1:04d}.CR2", "base": f"IMG_{N_IMAGES + 1:04d}",
                "rating": 4, "pick": 0, "label": "", "capture": "2021-02-02T10:00:00", "orient": "AB"})
    new.pop("global_", None)
    images2.append(new)
    cur = conn.cursor()
    insert_image(cur, new)
    conn.execute("COMMIT")
    wal_path = wal_work + "-wal"
    shutil.copy(wal_work, os.path.join(out, "synthetic-wal.lrcat"))
    shutil.copy(wal_path, os.path.join(out, "synthetic-wal.lrcat-wal"))
    committed_len = os.path.getsize(wal_path)
    # Uncommitted tail: a big transaction with a tiny page cache spills frames without a commit.
    conn.execute("PRAGMA cache_size=2")
    conn.execute("BEGIN")
    conn.execute("UPDATE Adobe_images SET colorLabels='Blue', touchCount=touchCount+1")
    conn.execute("UPDATE AgLibraryFile SET modTime=id_local")
    shutil.copy(wal_path, os.path.join(out, "synthetic-wal-uncommitted.lrcat-wal"))
    uncommitted_len = os.path.getsize(wal_path)
    conn.execute("ROLLBACK")
    conn.close()
    if uncommitted_len <= committed_len:
        sys.exit("no uncommitted frames were spilled; adjust the generator")
    tsv_dump(os.path.join(out, "synthetic-wal.expected.tsv"), images2, develop)
    shutil.rmtree(work)

    prov = {
        "generator": "generate_fixtures.py (hsk-studio-interop-lrcat tests/fixtures)",
        "purpose": "synthetic lrcat-shaped fixtures; byte-decoding proof only; not Lightroom-written; no personal data",
        "python": sys.version.split()[0],
        "sqlite_library_version": sqlite3.sqlite_version,
        "page_size": PAGE_SIZE,
        "images_main": N_IMAGES,
        "images_wal": N_IMAGES + 1,
        "wal_committed_len": committed_len,
        "wal_uncommitted_len": uncommitted_len,
        "files": {n: {"bytes": os.path.getsize(os.path.join(out, n)), "sha256": sha(os.path.join(out, n))}
                  for n in sorted(os.listdir(out)) if n != "PROVENANCE.json"},
    }
    with open(os.path.join(out, "PROVENANCE.json"), "w", newline="\n") as fh:
        json.dump(prov, fh, indent=2, sort_keys=True)
        fh.write("\n")
    print(json.dumps(prov["files"], indent=2))


if __name__ == "__main__":
    main()
