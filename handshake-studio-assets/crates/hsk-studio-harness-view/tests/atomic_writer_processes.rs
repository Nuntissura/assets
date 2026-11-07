//! Lane SMB-SESSION-WRITE-SAFETY-1: `export::write_atomic` and `audio_import::write_atomically` from separate OS
//! processes. The test binary re-executes itself (`child_entry`), so every writer has its own pid. A fixed temp name made
//! parallel writers overwrite or rename each other's temp file, and made a crash leftover or a parallel writer refuse
//! every later write (`view/export.rs`, `view/audio_import.rs`, HBR-SWARM-001).
#![cfg(feature = "viewer")]

use hsk_studio_harness_view::view::export::write_atomic;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const CHILD_DIR: &str = "HSK_WRITE_ATOMIC_CHILD_DIR";
const CHILD_ID: &str = "HSK_WRITE_ATOMIC_CHILD_ID";
#[cfg(feature = "media")]
const CHILD_AUDIO: &str = "HSK_WRITE_ATOMIC_CHILD_AUDIO";
const WRITERS: u8 = 6;
const ROUNDS: usize = 12;
/// Big enough that one write takes long enough to overlap the others.
const PAYLOAD: usize = 2 << 20;
const CHILD_DEADLINE: Duration = Duration::from_secs(120);

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hsk-write-atomic-{}-{label}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

fn child_command(dir: &Path, id: u8) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--ignored", "--exact", "child_entry", "--nocapture", "--test-threads=1"])
        .env(CHILD_DIR, dir)
        .env(CHILD_ID, id.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

fn spawn(dir: &Path, id: u8) -> Child {
    child_command(dir, id).spawn().unwrap()
}

/// The process body of every writer: exit 0 only when every write of every round returned Ok.
#[test]
#[ignore = "child process entry of this file's tests; proves nothing on its own"]
fn child_entry() {
    let Ok(dir) = std::env::var(CHILD_DIR) else {
        println!("NOT_PROVEN: child_entry is only the process body of the parallel writer tests");
        return;
    };
    let dir = PathBuf::from(dir);
    let id: u8 = std::env::var(CHILD_ID).unwrap().parse().unwrap();
    #[cfg(feature = "media")]
    if std::env::var(CHILD_AUDIO).is_ok() {
        audio::child_rounds(&dir, id);
        std::process::exit(0);
    }
    std::fs::write(dir.join(format!("ready-{id}")), b"").unwrap();
    let start = Instant::now();
    while !dir.join("go").exists() {
        assert!(start.elapsed() < CHILD_DEADLINE);
        std::thread::sleep(Duration::from_millis(1));
    }
    let payload = vec![id; PAYLOAD];
    let target = dir.join("out").join("result.bin");
    for _ in 0..ROUNDS {
        if write_atomic(&target, &payload).is_err() {
            std::process::exit(3);
        }
    }
    std::process::exit(0);
}

#[test]
fn parallel_processes_write_one_path_and_it_always_holds_one_whole_payload() {
    let scratch = Scratch::new("parallel");
    let out = scratch.0.join("out");
    std::fs::create_dir(&out).unwrap();
    let children: Vec<Child> = (1..=WRITERS).map(|id| spawn(&scratch.0, id)).collect();
    for id in 1..=WRITERS {
        let ready = scratch.0.join(format!("ready-{id}"));
        let start = Instant::now();
        while !ready.exists() {
            assert!(start.elapsed() < CHILD_DEADLINE);
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    std::fs::write(scratch.0.join("go"), b"").unwrap();
    let exits: Vec<Option<i32>> = children.into_iter().map(|mut c| c.wait().unwrap().code()).collect();
    assert!(exits.iter().all(|e| *e == Some(0)), "every write of every process landed: {exits:?}");
    let bytes = std::fs::read(out.join("result.bin")).unwrap();
    assert_eq!(bytes.len(), PAYLOAD, "one whole payload");
    assert!(bytes.iter().all(|b| *b == bytes[0]) && (1..=WRITERS).contains(&bytes[0]), "no torn file");
    assert_eq!(names(&out), ["result.bin"], "no temp file left behind");
}

/// A crash leftover under any earlier temp name is neither read, replaced nor a blocker.
#[test]
fn a_crash_leftover_is_harmless() {
    let scratch = Scratch::new("leftover");
    let target = scratch.0.join("out.png");
    // The old fixed name, as a directory, so a writer that still used it could not succeed.
    std::fs::create_dir(scratch.0.join(".out.png.export-tmp")).unwrap();
    let other = scratch.0.join(".hsk-export-1-0.tmp");
    std::fs::write(&other, b"left by a crash").unwrap();
    write_atomic(&target, b"payload").unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"payload");
    assert_eq!(std::fs::read(&other).unwrap(), b"left by a crash");
    assert_eq!(names(&scratch.0), [".hsk-export-1-0.tmp", ".out.png.export-tmp", "out.png"]);
}

/// Hostile paths refuse with the typed code; a name near the limit still works because the temp name does not embed it.
#[test]
fn hostile_output_paths_refuse_typed() {
    let scratch = Scratch::new("hostile");
    let near_limit = scratch.0.join("n".repeat(250));
    write_atomic(&near_limit, b"fits").unwrap();
    assert_eq!(std::fs::read(&near_limit).unwrap(), b"fits");
    let too_long = scratch.0.join("n".repeat(300));
    assert_eq!(write_atomic(&too_long, b"x"), Err("export_write"));
    let folder = scratch.0.join("folder");
    std::fs::create_dir(&folder).unwrap();
    assert_eq!(write_atomic(&folder, b"x"), Err("export_write"));
    assert_eq!(write_atomic(&scratch.0.join("missing").join("out.png"), b"x"), Err("export_write"));
    assert_eq!(write_atomic(Path::new(".."), b"x"), Err("export_path"));
    assert_eq!(names(&scratch.0).len(), 2, "only the near-limit file and the folder: {:?}", names(&scratch.0));
}

/// `write_atomically` (feature `media`): the same three properties.
#[cfg(feature = "media")]
mod audio {
    use super::*;
    use hsk_studio_harness_view::view::audio_import::write_atomically;
    use hsk_studio_harness_view::view::monitor::MonitorFail;

    fn write_all(output: &Path, overwrite: bool, bytes: &[u8]) -> Result<(), MonitorFail> {
        write_atomically(output, overwrite, |w| {
            use std::io::Write as _;
            w.write_all(bytes).map_err(|e| MonitorFail { code: "export_write", detail: e.to_string() })
        })
    }

    fn code(result: Result<(), MonitorFail>) -> &'static str {
        match result {
            Ok(()) => "ok",
            Err(fail) => fail.code,
        }
    }

    pub(super) fn child_rounds(dir: &Path, id: u8) {
        std::fs::write(dir.join(format!("ready-{id}")), b"").unwrap();
        let start = Instant::now();
        while !dir.join("go").exists() {
            assert!(start.elapsed() < CHILD_DEADLINE);
            std::thread::sleep(Duration::from_millis(1));
        }
        let payload = vec![id; PAYLOAD];
        let target = dir.join("out").join("mix.flac");
        for _ in 0..ROUNDS {
            if write_all(&target, true, &payload).is_err() {
                std::process::exit(3);
            }
        }
    }

    #[test]
    fn parallel_processes_write_one_audio_path_and_it_always_holds_one_whole_payload() {
        let scratch = Scratch::new("audio-parallel");
        let out = scratch.0.join("out");
        std::fs::create_dir(&out).unwrap();
        let children: Vec<Child> = (1..=WRITERS)
            .map(|id| {
                let mut command = child_command(&scratch.0, id);
                command.env(CHILD_AUDIO, "1");
                command.spawn().unwrap()
            })
            .collect();
        for id in 1..=WRITERS {
            let ready = scratch.0.join(format!("ready-{id}"));
            let start = Instant::now();
            while !ready.exists() {
                assert!(start.elapsed() < CHILD_DEADLINE);
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        std::fs::write(scratch.0.join("go"), b"").unwrap();
        let exits: Vec<Option<i32>> = children.into_iter().map(|mut c| c.wait().unwrap().code()).collect();
        assert!(exits.iter().all(|e| *e == Some(0)), "every write of every process landed: {exits:?}");
        let bytes = std::fs::read(out.join("mix.flac")).unwrap();
        assert_eq!(bytes.len(), PAYLOAD, "one whole payload");
        assert!(bytes.iter().all(|b| *b == bytes[0]) && (1..=WRITERS).contains(&bytes[0]), "no torn file");
        assert_eq!(names(&out), ["mix.flac"], "no temp file left behind");
    }

    /// A crash leftover under the earlier fixed name stays where it is and blocks nothing.
    #[test]
    fn an_audio_crash_leftover_is_harmless() {
        let scratch = Scratch::new("audio-leftover");
        let target = scratch.0.join("mix.flac");
        let leftover = scratch.0.join(".mix.flac.hsk-partial");
        std::fs::write(&leftover, b"left by a crash").unwrap();
        assert_eq!(code(write_all(&target, false, b"payload")), "ok");
        assert_eq!(std::fs::read(&target).unwrap(), b"payload");
        assert_eq!(std::fs::read(&leftover).unwrap(), b"left by a crash");
        assert_eq!(names(&scratch.0), [".mix.flac.hsk-partial", "mix.flac"]);
    }

    #[test]
    fn hostile_audio_output_paths_refuse_typed_and_leave_no_temp() {
        let scratch = Scratch::new("audio-hostile");
        let near_limit = scratch.0.join("n".repeat(250));
        assert_eq!(code(write_all(&near_limit, false, b"fits")), "ok");
        assert_eq!(code(write_all(&near_limit, false, b"again")), "output_exists");
        assert_eq!(code(write_all(&scratch.0.join("n".repeat(300)), false, b"x")), "export_write");
        let folder = scratch.0.join("folder");
        std::fs::create_dir(&folder).unwrap();
        assert_eq!(code(write_all(&folder, true, b"x")), "export_write");
        assert_eq!(code(write_all(&scratch.0.join("missing").join("x.flac"), false, b"x")), "output_path");
        assert_eq!(code(write_all(Path::new(".."), true, b"x")), "output_path");
        // A failing body removes its own temp.
        let refused = write_atomically(&scratch.0.join("body.flac"), false, |_| Err::<(), _>(MonitorFail { code: "export_write", detail: "body".to_string() }));
        assert_eq!(code(refused), "export_write");
        assert_eq!(names(&scratch.0).len(), 2, "only the near-limit file and the folder: {:?}", names(&scratch.0));
    }
}
