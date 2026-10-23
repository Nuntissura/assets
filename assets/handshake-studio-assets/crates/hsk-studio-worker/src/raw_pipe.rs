//! Sole-reader, nonblocking access to child output. No background readers are created.
use std::{io::{self, Read}, process::{Child, Command, Stdio}};

#[cfg(windows)]
pub(crate) fn same_environment_key(left: &std::ffi::OsStr, right: &std::ffi::OsStr) -> io::Result<bool> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Globalization::{CompareStringOrdinal, CSTR_EQUAL};
    let left: Vec<u16> = left.encode_wide().collect();
    let right: Vec<u16> = right.encode_wide().collect();
    let left_len = i32::try_from(left.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
    let right_len = i32::try_from(right.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
    // Both pointers borrow live UTF-16 buffers of the supplied lengths. Windows uses its ordinal
    // uppercase table; native keys need no Unicode conversion or lossy normalization.
    let compared = unsafe { CompareStringOrdinal(left.as_ptr(), left_len, right.as_ptr(), right_len, 1) };
    if compared == 0 { Err(io::Error::last_os_error()) } else { Ok(compared == CSTR_EQUAL) }
}

#[cfg(unix)]
pub(crate) struct Reader(std::os::unix::net::UnixStream);
#[cfg(windows)]
pub(crate) struct Reader(std::fs::File);
#[cfg(not(any(unix, windows)))]
pub(crate) struct Reader;

#[cfg(unix)]
pub(crate) struct Pipes(Reader, Reader);
#[cfg(not(unix))]
pub(crate) struct Pipes;

impl Pipes {
    pub(crate) fn prepare(command: &mut Command) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::{os::{fd::OwnedFd, unix::net::UnixStream}};
            let (stdout, child_stdout) = UnixStream::pair()?;
            let (stderr, child_stderr) = UnixStream::pair()?;
            stdout.set_nonblocking(true)?;
            stderr.set_nonblocking(true)?;
            command.stdout(Stdio::from(OwnedFd::from(child_stdout)))
                .stderr(Stdio::from(OwnedFd::from(child_stderr)));
            Ok(Self(Reader(stdout), Reader(stderr)))
        }
        #[cfg(windows)]
        {
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
            Ok(Self)
        }
        #[cfg(not(any(unix, windows)))]
        { let _ = command; Err(io::ErrorKind::Unsupported.into()) }
    }

    pub(crate) fn readers(self, child: &mut Child) -> io::Result<(Reader, Reader)> {
        #[cfg(unix)]
        { let _ = child; Ok((self.0, self.1)) }
        #[cfg(windows)]
        {
            use std::os::windows::io::OwnedHandle;
            let stdout = child.stdout.take().ok_or(io::ErrorKind::BrokenPipe)?;
            let stderr = child.stderr.take().ok_or(io::ErrorKind::BrokenPipe)?;
            Ok((Reader(std::fs::File::from(OwnedHandle::from(stdout))),
                Reader(std::fs::File::from(OwnedHandle::from(stderr)))))
        }
        #[cfg(not(any(unix, windows)))]
        { let _ = child; Err(io::ErrorKind::Unsupported.into()) }
    }
}

impl Reader {
    pub(crate) fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        #[cfg(unix)]
        { self.0.read(buffer) }
        #[cfg(windows)]
        { try_read_native_pipe(&mut self.0, buffer) }
        #[cfg(not(any(unix, windows)))]
        { let _ = buffer; Err(io::ErrorKind::Unsupported.into()) }
    }
}

/// Nonblocking Windows pipe read for a creation-time process adapter. The caller must be the sole
/// reader of this native pipe, including any duplicated handles. Ordinary files are unsupported.
#[cfg(windows)]
pub fn try_read_native_pipe(pipe: &mut std::fs::File, buffer: &mut [u8]) -> io::Result<usize> {
    use std::{os::windows::io::AsRawHandle, ptr};
    use windows_sys::Win32::{Foundation::{ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED}, System::Pipes::PeekNamedPipe};
    let mut available = 0;
    // With one reader and bytes available, Read cannot wait for a subsequent writer.
    let ok = unsafe { PeekNamedPipe(pipe.as_raw_handle(), ptr::null_mut(), 0,
        ptr::null_mut(), &mut available, ptr::null_mut()) };
    if ok == 0 {
        let error = io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(code) if code == ERROR_BROKEN_PIPE as i32 || code == ERROR_PIPE_NOT_CONNECTED as i32) {
            return Ok(0);
        }
        return Err(error);
    }
    if available == 0 { return Err(io::ErrorKind::WouldBlock.into()); }
    let length = buffer.len().min(usize::try_from(available).unwrap_or(usize::MAX));
    pipe.read(&mut buffer[..length])
}
