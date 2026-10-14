//! Worker-program lookup: D14, worker executables ship beside the host executable and a consumer may
//! point at another program (`with_worker_program`).
#![deny(clippy::arithmetic_side_effects)]
use std::path::{Path, PathBuf};

/// `program_name` (plus the platform executable suffix) in the directory of `exe` and nowhere else. A
/// parent directory is not searched: a planted binary one level up must not become the worker
/// (callers that run from `deps/` or `examples/` pass the program explicitly).
pub fn find_beside(exe: &Path, program_name: &str) -> Option<PathBuf> {
    let name = format!("{program_name}{}", std::env::consts::EXE_SUFFIX);
    Some(exe.parent()?.join(name)).filter(|candidate| candidate.is_file())
}

/// [`find_beside`] for the running executable.
pub fn discover_program(program_name: &str) -> Option<PathBuf> {
    find_beside(&std::env::current_exe().ok()?, program_name)
}

/// The explicit program if given, else the one beside the running executable; `None` unless the
/// result is an existing file.
pub fn resolve_program(explicit: Option<PathBuf>, program_name: &str) -> Option<PathBuf> {
    explicit
        .or_else(|| discover_program(program_name))
        .filter(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A worker file planted in the directory ABOVE the host executable's own directory is never
    /// selected; a directory named like the worker is not a program; only the executable's directory is.
    #[test]
    fn worker_is_found_only_beside_the_running_executable() {
        let root = std::env::temp_dir().join(format!("hsk-worker-discover-{}", std::process::id()));
        let _guard = Scratch(root.clone());
        let bin = root.join("bin");
        fs::create_dir_all(&bin).expect("scratch directory");
        let program = "hsk-planted-worker";
        let name = format!("{program}{}", std::env::consts::EXE_SUFFIX);
        let host = bin.join(format!("host{}", std::env::consts::EXE_SUFFIX));

        fs::write(root.join(&name), b"decoy above the executable directory").expect("decoy");
        assert_eq!(find_beside(&host, program), None, "the parent directory must not be searched");
        fs::create_dir_all(bin.join(&name)).expect("a directory named like the worker");
        assert_eq!(find_beside(&host, program), None, "a directory is not a worker program");
        fs::remove_dir_all(bin.join(&name)).expect("remove the directory");
        fs::write(bin.join(&name), b"beside the executable").expect("worker beside the host");
        assert_eq!(find_beside(&host, program), Some(bin.join(&name)));
        assert_eq!(find_beside(Path::new(""), program), None, "no parent, no lookup");
        let explicit = bin.join(&name);
        assert_eq!(resolve_program(Some(explicit.clone()), program), Some(explicit));
        assert_eq!(resolve_program(Some(root.join("missing")), program), None);
    }
}
