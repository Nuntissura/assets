//! Interop-local real consumer: decode a Lightroom catalog file into import proposals and print
//! the counts and the loss summary. Files are opened read-only and never written; a refusal
//! prints a stable error code on stderr and exits 2.
//!   --descriptor
//!   --main FILE [--wal FILE | --wal-present-ungranted]
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_lrcat::*;
use std::{env, fs::File, process::ExitCode};

struct FileSource {
    file: File,
    len: u64,
}

impl FileSource {
    fn open(path: &str) -> std::io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self { file, len })
    }
}

impl ByteSource for FileSource {
    fn len(&self) -> u64 {
        self.len
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::result::Result<(), SourceError> {
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or(SourceError::OutOfRange)?;
        if end > self.len {
            return Err(SourceError::OutOfRange);
        }
        let mut done = 0usize;
        while done < buf.len() {
            let at = offset + done as u64;
            #[cfg(windows)]
            let n = std::os::windows::fs::FileExt::seek_read(&self.file, &mut buf[done..], at);
            #[cfg(unix)]
            let n = std::os::unix::fs::FileExt::read_at(&self.file, &mut buf[done..], at);
            match n {
                Ok(0) | Err(_) => return Err(SourceError::Unavailable),
                Ok(n) => done += n,
            }
        }
        Ok(())
    }
}

fn arg<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).map(String::as_str)
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.iter().any(|a| a == "--descriptor") {
        println!("{DESCRIPTOR}");
        return ExitCode::SUCCESS;
    }
    let Some(main_path) = arg(&args, "--main") else {
        eprintln!(
            "usage: interop-lrcat-consumer [--descriptor] --main FILE [--wal FILE | --wal-present-ungranted]"
        );
        return ExitCode::from(2);
    };
    let Ok(main) = FileSource::open(main_path) else {
        eprintln!("unreadable_input");
        return ExitCode::from(2);
    };
    let wal = if args.iter().any(|a| a == "--wal-present-ungranted") {
        WalGrant::PresentButNotGranted
    } else if let Some(path) = arg(&args, "--wal") {
        match FileSource::open(path) {
            Ok(w) => WalGrant::Granted(Box::new(w)),
            Err(_) => {
                eprintln!("unreadable_input");
                return ExitCode::from(2);
            }
        }
    } else {
        WalGrant::None
    };
    let grant = Grant {
        main: Box::new(main),
        wal,
    };
    let cancel = CancellationToken::default();
    let proposal = match extract_catalog(&grant, &Limits::default(), &cancel) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{}", e.code());
            return ExitCode::from(2);
        }
    };
    println!(
        "photos={} develop={} collections={} members={} keywords={} assignments={} schema_version={:?}",
        proposal.photos.len(),
        proposal.develop.len(),
        proposal.collections.len(),
        proposal.collection_members.len(),
        proposal.keywords.len(),
        proposal.keyword_assignments.len(),
        proposal.schema_version
    );
    println!(
        "main_sha256={} main_len={} wal_len={} committed_wal_len={} reconciles={}",
        proposal.fingerprint.main_sha256_hex(),
        proposal.fingerprint.main_len,
        proposal.fingerprint.wal_len,
        proposal.fingerprint.committed_wal_len,
        proposal.reconciles()
    );
    for c in &proposal.counts {
        println!("count {} seen={} mapped={} lost={}", c.entity, c.rows_seen, c.mapped, c.lost);
    }
    for l in &proposal.losses {
        println!(
            "loss {} {} {} {} count={}",
            l.disposition.wire(),
            l.entity,
            l.field,
            l.code,
            l.count
        );
    }
    println!("notice {}", proposal.notice);
    ExitCode::SUCCESS
}
