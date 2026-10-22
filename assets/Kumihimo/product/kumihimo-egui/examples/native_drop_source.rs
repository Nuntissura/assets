//! Bounded actual OLE source for an explicitly selected fixture; never positions the cursor.
//! Requires an explicit observed receiver HWND/PID and its live registration marker.
#[cfg(all(windows, feature = "native-drops"))]
mod source {
    use std::{sync::{Arc, atomic::{AtomicBool, Ordering}}, time::{Instant, Duration}, path::{Path, PathBuf, Component, Prefix}, ffi::OsString, os::windows::ffi::{OsStrExt, OsStringExt}};
    use windows::{core::{implement, BOOL, HRESULT, HSTRING}, Win32::{
        Foundation::{DRAGDROP_S_DROP, DRAGDROP_S_CANCEL, DRAGDROP_S_USEDEFAULTCURSORS, HWND, HANDLE, FILETIME, POINT, WAIT_TIMEOUT, CloseHandle},
        System::{Com::{IDataObject, IBindCtx}, Ole::{IDropSource, IDropSource_Impl, OleInitialize,
            OleUninitialize, DoDragDrop, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE},
            SystemServices::MODIFIERKEYS_FLAGS,
            Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, GetProcessTimes, WaitForSingleObject}},
        UI::{Shell::{IShellItem, SHCreateItemFromParsingName, BHID_DataObject},
            WindowsAndMessaging::{GetCursorPos, WindowFromPoint, GetAncestor, GA_ROOT, GetWindowThreadProcessId, GetPropW, IsWindow}},
    }};
    use kumihimo_egui::native_drop::RECEIVER_LIFETIME_PROPERTY;
    fn stage_ok(stage: &'static str) { eprintln!("{{\"stage\":\"{stage}\",\"status\":\"ok\"}}"); }
    fn win_stage<T>(stage: &'static str, result: windows::core::Result<T>) -> Result<T, Box<dyn std::error::Error>> {
        result.map(|value| { stage_ok(stage); value }).map_err(|error| {
            eprintln!("{{\"stage\":\"{stage}\",\"status\":\"failed\",\"hresult\":\"0x{:08X}\"}}", error.code().0 as u32);
            format!("Native drop source stage={stage} HRESULT=0x{:08X}", error.code().0 as u32).into()
        })
    }
    fn io_stage<T>(stage: &'static str, result: std::io::Result<T>) -> Result<T, Box<dyn std::error::Error>> {
        result.map_err(|error| {
            let os_error = error.raw_os_error().map(|value| value.to_string()).unwrap_or_else(|| "null".into());
            eprintln!("{{\"stage\":\"{stage}\",\"status\":\"failed\",\"os_error\":{os_error},\"kind\":\"{:?}\"}}", error.kind());
            format!("Native drop source stage={stage} kind={:?} OS={:?}", error.kind(), error.raw_os_error()).into()
        })
    }
    /// Shell parsing and file-I/O namespaces differ (Microsoft maximum-file-path-limitation).
    /// Support bounded ordinary drive/UNC names; re-canonicalize before accepting conversion.
    fn shell_path(path: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        let converted = match path.components().next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::VerbatimDisk(_) => wide[4..].to_vec(),
                Prefix::VerbatimUNC(_, _) => [vec![92_u16, 92], wide[8..].to_vec()].concat(),
                Prefix::Disk(_) | Prefix::UNC(_, _) => wide,
                _ => return Err("Native drop source stage=shell_path unsupported fixture namespace".into()),
            },
            _ => return Err("Native drop source stage=shell_path absolute fixture required".into()),
        };
        if converted.contains(&0) || converted.len() >= 260 { return Err("Native drop source stage=shell_path fixture exceeds bounded Shell parsing name".into()); }
        let ordinary = PathBuf::from(OsString::from_wide(&converted));
        let checked = io_stage("shell_path_recanonicalize", std::fs::canonicalize(&ordinary))?;
        if checked != path { return Err("Native drop source stage=shell_path conversion changed canonical fixture identity".into()); }
        stage_ok("shell_path");
        Ok(ordinary)
    }
    struct ExpectedReceiver { hwnd: isize, pid: u32, process: isize, creation: FILETIME, marker: usize }
    impl ExpectedReceiver {
        fn capture(hwnd: isize, pid: u32) -> Result<Self, Box<dyn std::error::Error>> {
            if hwnd == 0 || pid == 0 { return Err("Receiver HWND/PID must be nonzero".into()); }
            let process = win_stage("receiver_open_process", unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, false, pid) })?;
            let mut receiver = Self { hwnd, pid, process: process.0 as isize, creation: FILETIME::default(), marker: 0 };
            let mut exit = FILETIME::default(); let mut kernel = FILETIME::default(); let mut user = FILETIME::default();
            win_stage("receiver_process_times", unsafe { GetProcessTimes(process, &mut receiver.creation, &mut exit, &mut kernel, &mut user) })?;
            receiver.marker = unsafe { GetPropW(HWND(hwnd as *mut _), RECEIVER_LIFETIME_PROPERTY) }.0 as usize;
            if receiver.marker == 0 || !receiver.valid_under_cursor() { return Err("Native drop source stage=receiver_capture expected registered sample is not live root under cursor".into()); }
            stage_ok("receiver_capture");
            Ok(receiver)
        }
        fn valid_under_cursor(&self) -> bool {
            let window = HWND(self.hwnd as *mut _); let process = HANDLE(self.process as *mut _);
            let mut pid = 0; let mut point = POINT::default();
            let mut creation = FILETIME::default(); let mut exit = FILETIME::default(); let mut kernel = FILETIME::default(); let mut user = FILETIME::default();
            unsafe {
                if WaitForSingleObject(process, 0) != WAIT_TIMEOUT || !IsWindow(Some(window)).as_bool()
                    || GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user).is_err()
                    || creation != self.creation || GetCursorPos(&mut point).is_err() { return false; }
                GetWindowThreadProcessId(window, Some(&mut pid));
                if pid != self.pid || GetAncestor(window, GA_ROOT) != window
                    || GetPropW(window, RECEIVER_LIFETIME_PROPERTY).0 as usize != self.marker { return false; }
                let actual = GetAncestor(WindowFromPoint(point), GA_ROOT);
                let mut actual_pid = 0; GetWindowThreadProcessId(actual, Some(&mut actual_pid));
                actual == window && actual_pid == self.pid
                    && GetPropW(actual, RECEIVER_LIFETIME_PROPERTY).0 as usize == self.marker
            }
        }
    }
    impl Drop for ExpectedReceiver { fn drop(&mut self) { unsafe { let _ = CloseHandle(HANDLE(self.process as *mut _)); } } }
    #[implement(IDropSource)]
    struct Source { started: Instant, receiver: Arc<ExpectedReceiver>, accepted: AtomicBool, cancelled: AtomicBool }
    #[allow(non_snake_case)]
    impl IDropSource_Impl for Source_Impl {
        fn QueryContinueDrag(&self, escape: BOOL, _: MODIFIERKEYS_FLAGS) -> HRESULT {
            if escape.as_bool() || self.started.elapsed() > Duration::from_secs(3)
                || self.cancelled.load(Ordering::Relaxed) || !self.receiver.valid_under_cursor() { DRAGDROP_S_CANCEL }
            else if self.accepted.load(Ordering::Relaxed) { DRAGDROP_S_DROP }
            else { HRESULT(0) }
        }
        fn GiveFeedback(&self, effect: DROPEFFECT) -> HRESULT {
            if !self.receiver.valid_under_cursor() || self.started.elapsed() > Duration::from_secs(3) {
                self.accepted.store(false, Ordering::Relaxed);
                self.cancelled.store(true, Ordering::Relaxed);
                // QueryContinueDrag performs OLE cancellation; no later COPY can release.
                return DRAGDROP_S_USEDEFAULTCURSORS;
            }
            self.accepted.store(effect.0 & DROPEFFECT_COPY.0 != 0, Ordering::Relaxed);
            DRAGDROP_S_USEDEFAULTCURSORS
        }
    }
    struct Ole;
    impl Drop for Ole { fn drop(&mut self) { unsafe { OleUninitialize(); } } }
    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let mut fixture = None; let mut hwnd = None; let mut pid = None;
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--fixture" if fixture.is_none() => fixture = Some(args.next().ok_or("Missing fixture path")?),
                "--receiver-hwnd" if hwnd.is_none() => hwnd = Some(args.next().ok_or("Missing receiver HWND")?.parse::<isize>()?),
                "--receiver-pid" if pid.is_none() => pid = Some(args.next().ok_or("Missing receiver PID")?.parse::<u32>()?),
                _ => return Err("Use --fixture PATH --receiver-hwnd DECIMAL --receiver-pid PID; duplicate/unknown flags rejected".into()),
            }
        }
        let path=io_stage("fixture_canonicalize", std::fs::canonicalize(fixture.ok_or("Explicit --fixture required")?))?;
        let metadata=io_stage("fixture_metadata", std::fs::metadata(&path))?;
        if !metadata.is_file() || metadata.len()>8192 { return Err("Native drop source stage=fixture_validation regular fixture <=8KiB required".into()); }
        stage_ok("fixture_validation");
        let receiver=Arc::new(ExpectedReceiver::capture(hwnd.ok_or("Explicit --receiver-hwnd required")?,pid.ok_or("Explicit --receiver-pid required")?)?);
        win_stage("ole_initialize", unsafe { OleInitialize(None) })?;
        let _ole=Ole;
        let parsing_name=shell_path(&path)?;
        let item:IShellItem=win_stage("shell_create_item", unsafe { SHCreateItemFromParsingName(&HSTRING::from(parsing_name.as_os_str()), None::<&IBindCtx>) })?;
        let data:IDataObject=win_stage("shell_bind_data_object", unsafe { item.BindToHandler(None::<&IBindCtx>, &BHID_DataObject) })?;
        let source:IDropSource=Source { started:Instant::now(), receiver:receiver.clone(), accepted:AtomicBool::new(false), cancelled:AtomicBool::new(false) }.into();
        let mut effect=DROPEFFECT_NONE;
        if !receiver.valid_under_cursor() { return Err("Native drop source stage=pre_drag_guard receiver lifetime/actual cursor target changed".into()); }
        stage_ok("pre_drag_guard");
        let outcome=unsafe { DoDragDrop(&data,&source,DROPEFFECT_COPY,&mut effect) };
        println!("{{\"boundary\":\"actual-shell-ole-transfer\",\"hresult\":{},\"effect\":{},\"receiver_hwnd\":{},\"receiver_pid\":{},\"receiver_marker\":{},\"host_import\":\"unproven\"}}",outcome.0,effect.0,receiver.hwnd,receiver.pid,receiver.marker);
        if outcome!=DRAGDROP_S_DROP || effect!=DROPEFFECT_COPY || !receiver.valid_under_cursor() { return Err("Expected receiver copy transfer not confirmed; sample document outcome remains unproven".into()); }
        Ok(())
    }
}
fn main() {
    #[cfg(all(windows, feature = "native-drops"))]
    if let Err(error)=source::run() { eprintln!("{error}");std::process::exit(1); }
    #[cfg(not(all(windows, feature = "native-drops")))]
    eprintln!("native_drop_source requires native,drops on Windows");
}
