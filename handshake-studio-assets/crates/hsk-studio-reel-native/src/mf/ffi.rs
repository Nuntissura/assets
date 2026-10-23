//! The only module of this crate with `unsafe` (feature `mf-worker`, Windows): Media Foundation through
//! the `windows` crate bindings (public COM interfaces of OS components; nothing is bundled). Every call
//! here returns typed results; pixel bytes leave this module as an owned copy and are validated by the
//! pure `nv12` module. No window, no swap chain, no Direct3D device: the software path only (slice 1).
//!
//! Sources (Microsoft Learn, opened by the lane brief): MFStartup, MFTEnumEx, Basic MFT processing model,
//! IMFTransform::ProcessOutput, H.264 video decoder, Uncompressed video buffers.
#![allow(unsafe_code)]
#![deny(clippy::arithmetic_side_effects)]
use std::mem::ManuallyDrop;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFMediaType, IMFSample, IMFTransform, MF_E_NO_MORE_TYPES, MF_E_NOTACCEPTING,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_MT_DEFAULT_STRIDE,
    MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_SUBTYPE,
    MF_TRANSFORM_ASYNC, MF_VERSION, MFCreateMediaType, MFCreateMemoryBuffer,
    MFCreateSample, MFMediaType_Video, MFSTARTUP_LITE, MFShutdown, MFStartup,
    MFT_CATEGORY_VIDEO_DECODER, MFT_ENUM_FLAG_ASYNCMFT, MFT_ENUM_FLAG_HARDWARE,
    MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT, MFT_ENUM_HARDWARE_URL_Attribute,
    MFT_FRIENDLY_NAME_Attribute, MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    MFT_REGISTER_TYPE_INFO, MFT_TRANSFORM_CLSID_Attribute, MFVideoFormat_H264, MFVideoFormat_NV12,
    MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, MF_MT_AAC_PAYLOAD_TYPE, MF_MT_AUDIO_BITS_PER_SAMPLE,
    MF_MT_AUDIO_CHANNEL_MASK, MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND,
    MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_INTERLACE_MODE, MF_MT_MPEG2_PROFILE,
    MF_MT_PIXEL_ASPECT_RATIO, MF_MT_USER_DATA, MFAudioFormat_AAC, MFAudioFormat_Float,
    MFAudioFormat_PCM, MFMediaType_Audio, MFSampleExtension_CleanPoint, MFT_CATEGORY_AUDIO_DECODER,
    MFT_CATEGORY_VIDEO_ENCODER, MFVideoFormat_HEVC, MFVideoInterlace_Progressive,
};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
    ID3D11Multithread,
};
use windows::Win32::Graphics::Dxgi::IDXGIAdapter;
use windows::Win32::Media::MediaFoundation::{
    IMFDXGIBuffer, IMFDXGIDeviceManager, MF_E_UNSUPPORTED_D3D_TYPE, MFCreateDXGIDeviceManager,
    MFT_MESSAGE_SET_D3D_MANAGER,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::core::{GUID, HRESULT, Interface, PWSTR};

/// Media Foundation started on this thread (`CoInitializeEx` multithreaded + `MFStartup` lite). Dropped
/// once: `MFShutdown`, then `CoUninitialize`.
pub(crate) struct Runtime {
    _not_send: std::marker::PhantomData<*const ()>,
}

impl Runtime {
    /// `Err(hresult)` when COM or Media Foundation cannot start (`E_NOTIMPL` when the media components
    /// are absent, `MF_E_DISABLED_IN_SAFEMODE` in safe mode; Learn MFStartup).
    pub(crate) fn start() -> Result<Self, i32> {
        // SAFETY: called once on the worker's decode thread before any other COM call; no reserved pointer.
        let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if com.is_err() {
            return Err(com.0);
        }
        // SAFETY: COM is initialised on this thread; MF_VERSION/MFSTARTUP_LITE are the documented arguments.
        if let Err(error) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) } {
            // SAFETY: balances the successful CoInitializeEx above on the same thread.
            unsafe { CoUninitialize() };
            return Err(error.code().0);
        }
        Ok(Self {
            _not_send: std::marker::PhantomData,
        })
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // SAFETY: one MFShutdown per successful MFStartup, then one CoUninitialize per CoInitializeEx, on
        // the thread that started them (Runtime is not Send).
        unsafe {
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}

/// A headless Direct3D 11 video device shared with Media Foundation through a DXGI device manager (Learn
/// "Supporting Direct3D 11 video decoding in Media Foundation"): no window, no swap chain.
pub(crate) struct Gpu {
    _device: ID3D11Device,
    manager: IMFDXGIDeviceManager,
}

/// Creates the hardware D3D11 device with `D3D11_CREATE_DEVICE_VIDEO_SUPPORT`, turns on multithread
/// protection and hands it to a new DXGI device manager. `Err(hresult)` when this host has no such device.
pub(crate) fn create_gpu() -> Result<Gpu, i32> {
    let mut device: Option<ID3D11Device> = None;
    // SAFETY: default adapter, hardware driver, no software module, out pointer to a local; no window.
    unsafe {
        D3D11CreateDevice(
            None::<&IDXGIAdapter>,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
    }
    .map_err(|error| error.code().0)?;
    let device = device.ok_or(0)?;
    if let Ok(multithread) = device.cast::<ID3D11Multithread>() {
        // SAFETY: plain flag set on the device the decoder and this thread share.
        let _ = unsafe { multithread.SetMultithreadProtected(true) };
    }
    let mut token = 0_u32;
    let mut manager: Option<IMFDXGIDeviceManager> = None;
    // SAFETY: out pointers to locals.
    unsafe { MFCreateDXGIDeviceManager(&mut token, &mut manager) }.map_err(|error| error.code().0)?;
    let manager = manager.ok_or(0)?;
    // SAFETY: binds the live device to the manager with the token the manager issued.
    unsafe { manager.ResetDevice(&device, token) }.map_err(|error| error.code().0)?;
    Ok(Gpu {
        _device: device,
        manager,
    })
}

/// One H.264 decoder MFT as `MFTEnumEx` lists it.
pub(crate) struct DecoderEntry {
    pub name: String,
    pub clsid: GUID,
    pub hardware: bool,
    pub asynchronous: bool,
    activate: IMFActivate,
}

impl DecoderEntry {
    /// CLSID in registry form `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`.
    pub(crate) fn clsid_text(&self) -> String {
        format!("{{{:?}}}", self.clsid)
    }
}

fn read_string(activate: &IMFActivate, key: &GUID) -> Option<String> {
    let mut text = PWSTR::null();
    let mut len = 0_u32;
    // SAFETY: on success the attribute store allocates `text` with CoTaskMemAlloc and writes its length in
    // UTF-16 units (without the terminator); it is read once and freed below.
    unsafe { activate.GetAllocatedString(key, &mut text, &mut len) }.ok()?;
    if text.is_null() {
        return None;
    }
    // SAFETY: `text` points to `len` UTF-16 units written by the call above (still allocated).
    let units = unsafe { std::slice::from_raw_parts(text.0, usize::try_from(len).ok()?) };
    let value = String::from_utf16_lossy(units);
    // SAFETY: frees the CoTaskMemAlloc block returned by GetAllocatedString exactly once.
    unsafe { CoTaskMemFree(Some(text.0 as *const _)) };
    Some(value)
}

/// `MFTEnumEx(MFT_CATEGORY_VIDEO_DECODER, SYNC | ASYNC | HARDWARE | SORTANDFILTER, {Video, H264}, NULL)`:
/// every H.264 decoder MFT in the order Media Foundation ranks them. Zero matches is an empty list (the
/// call itself still succeeds, Learn MFTEnumEx).
pub(crate) fn enumerate_h264_decoders() -> Result<Vec<DecoderEntry>, i32> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    enumerate_mfts(MFT_CATEGORY_VIDEO_DECODER, Some(&input), None)
}

/// The AAC decoder MFTs (`MFT_CATEGORY_AUDIO_DECODER`, input `{Audio, AAC}`), ranked (lane
/// SMB-D24A-CODEC-DEFAULTS, D24-A).
pub(crate) fn enumerate_aac_decoders() -> Result<Vec<DecoderEntry>, i32> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Audio,
        guidSubtype: MFAudioFormat_AAC,
    };
    enumerate_mfts(MFT_CATEGORY_AUDIO_DECODER, Some(&input), None)
}

/// The video encoder MFTs whose output is `subtype` (H.264 or HEVC), ranked (feature `os-encode`, D24-A).
#[cfg_attr(not(feature = "os-encode"), allow(dead_code))]
pub(crate) fn enumerate_video_encoders(subtype: &GUID) -> Result<Vec<DecoderEntry>, i32> {
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: *subtype,
    };
    enumerate_mfts(MFT_CATEGORY_VIDEO_ENCODER, None, Some(&output))
}

fn enumerate_mfts(
    category: GUID,
    input: Option<&MFT_REGISTER_TYPE_INFO>,
    output: Option<&MFT_REGISTER_TYPE_INFO>,
) -> Result<Vec<DecoderEntry>, i32> {
    let flags = MFT_ENUM_FLAG_SYNCMFT
        | MFT_ENUM_FLAG_ASYNCMFT
        | MFT_ENUM_FLAG_HARDWARE
        | MFT_ENUM_FLAG_SORTANDFILTER;
    let mut array: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0_u32;
    // SAFETY: `input` outlives the call; the out pointers are valid locals.
    unsafe {
        windows::Win32::Media::MediaFoundation::MFTEnumEx(
            category,
            flags,
            input.map(std::ptr::from_ref),
            output.map(std::ptr::from_ref),
            &mut array,
            &mut count,
        )
    }
    .map_err(|error| error.code().0)?;
    let mut entries = Vec::new();
    if array.is_null() {
        return Ok(entries);
    }
    let len = usize::try_from(count).unwrap_or(0);
    // SAFETY: MFTEnumEx returned `count` activation pointers in one CoTaskMemAlloc array; each slot is
    // taken (ownership moves to `entries`, released on drop) and the array is freed once below.
    let slots = unsafe { std::slice::from_raw_parts_mut(array, len) };
    for slot in slots.iter_mut() {
        let Some(activate) = slot.take() else { continue };
        let name = read_string(&activate, &MFT_FRIENDLY_NAME_Attribute).unwrap_or_default();
        // SAFETY: plain attribute reads on a live activation object.
        let clsid = unsafe { activate.GetGUID(&MFT_TRANSFORM_CLSID_Attribute) }.unwrap_or_default();
        // SAFETY: as above.
        let hardware = unsafe { activate.GetStringLength(&MFT_ENUM_HARDWARE_URL_Attribute) }.is_ok();
        // SAFETY: as above.
        let asynchronous = unsafe { activate.GetUINT32(&MF_TRANSFORM_ASYNC) }.unwrap_or(0) != 0;
        entries.push(DecoderEntry {
            name,
            clsid,
            hardware,
            asynchronous,
            activate,
        });
    }
    // SAFETY: frees the array MFTEnumEx allocated; every slot was emptied above.
    unsafe { CoTaskMemFree(Some(array as *const _)) };
    Ok(entries)
}

/// Why a decoder call failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MfFail {
    /// No NV12 output type offered.
    NoNv12,
    /// The decoder refused the Direct3D manager (`MF_E_UNSUPPORTED_D3D_TYPE`).
    D3dRefused,
    /// A second output format change after pictures were released in this segment.
    FormatChange,
    /// Any other failed HRESULT (the call that failed is the context).
    Call(&'static str, i32),
}

/// Output picture straight from the decoder: an owned copy of the contiguous NV12 buffer plus its layout.
pub(crate) struct RawPicture {
    pub time: i64,
    pub bytes: Vec<u8>,
    /// Frame size of the current output type (buffer layout: luma rows = `height`).
    pub width: u32,
    pub height: u32,
    /// `MF_MT_DEFAULT_STRIDE` of the output type when set (may be negative = bottom-up).
    pub default_stride: Option<i32>,
    /// `MF_MT_MINIMUM_DISPLAY_APERTURE` area `(width, height)` when set.
    pub aperture: Option<(i32, i32)>,
    /// The decoder released a Direct3D surface (`IMFDXGIBuffer`): the picture was decoded through DXVA.
    pub gpu_surface: bool,
}

struct OutLayout {
    width: u32,
    height: u32,
    default_stride: Option<i32>,
    aperture: Option<(i32, i32)>,
}

/// One synchronous software decoder MFT for one segment.
pub(crate) struct Decoder {
    mft: IMFTransform,
    provides_samples: bool,
    sample_bytes: u32,
    layout: OutLayout,
}

fn call<T>(what: &'static str, result: windows::core::Result<T>) -> Result<T, MfFail> {
    result.map_err(|error| MfFail::Call(what, error.code().0))
}

fn code_of<T>(result: &windows::core::Result<T>) -> Option<HRESULT> {
    result.as_ref().err().map(windows::core::Error::code)
}

fn video_type(subtype: &GUID, size: Option<(u32, u32)>) -> Result<IMFMediaType, MfFail> {
    // SAFETY: plain COM object creation and attribute writes on the new object.
    unsafe {
        let media = call("MFCreateMediaType", MFCreateMediaType())?;
        call("SetGUID", media.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video))?;
        call("SetGUID", media.SetGUID(&MF_MT_SUBTYPE, subtype))?;
        if let Some((width, height)) = size {
            let packed = (u64::from(width) << 32) | u64::from(height);
            call("SetUINT64", media.SetUINT64(&MF_MT_FRAME_SIZE, packed))?;
        }
        Ok(media)
    }
}

impl Decoder {
    /// Activates `entry`, hands it the Direct3D manager when `gpu` is given (DXVA), sets the H.264 input
    /// type (with the container-declared frame size when known) and the NV12 output type, and starts
    /// streaming. A decoder that cannot use the manager answers `D3dRefused` (the caller decides on a
    /// software decoder; no silent switch inside a hardware request).
    pub(crate) fn open(
        entry: &DecoderEntry,
        size: Option<(u32, u32)>,
        gpu: Option<&Gpu>,
    ) -> Result<Self, MfFail> {
        // SAFETY: activation of an object MFTEnumEx returned; the result is an owned interface.
        let mft: IMFTransform = call("ActivateObject", unsafe { entry.activate.ActivateObject() })?;
        if let Some(gpu) = gpu {
            // SAFETY: the message parameter is the manager's IUnknown pointer, alive for the decoder's
            // lifetime (the Gpu outlives every Decoder of the worker; the MFT takes its own reference).
            let sent = unsafe {
                mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, gpu.manager.as_raw() as usize)
            };
            if sent.is_err() {
                return Err(MfFail::D3dRefused);
            }
        }
        let input = video_type(&MFVideoFormat_H264, size)?;
        // SAFETY: stream 0 of a decoder MFT; `input` is a complete video type.
        let accepted = unsafe { mft.SetInputType(0, &input, 0) };
        if gpu.is_some() && code_of(&accepted) == Some(MF_E_UNSUPPORTED_D3D_TYPE) {
            return Err(MfFail::D3dRefused);
        }
        call("SetInputType", accepted)?;
        let mut decoder = Self {
            mft,
            provides_samples: false,
            sample_bytes: 0,
            layout: OutLayout {
                width: 0,
                height: 0,
                default_stride: None,
                aperture: None,
            },
        };
        decoder.negotiate_nv12()?;
        // SAFETY: documented start-up messages of the basic processing model; no parameter.
        unsafe {
            call(
                "ProcessMessage",
                decoder.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0),
            )?;
            call(
                "ProcessMessage",
                decoder.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0),
            )?;
        }
        Ok(decoder)
    }

    /// Picks the first NV12 output type the decoder offers and reads its layout (also after a stream
    /// change).
    fn negotiate_nv12(&mut self) -> Result<(), MfFail> {
        for index in 0..64_u32 {
            // SAFETY: enumeration of the output types of stream 0.
            let offered = unsafe { self.mft.GetOutputAvailableType(0, index) };
            if code_of(&offered) == Some(MF_E_NO_MORE_TYPES) {
                break;
            }
            let offered = call("GetOutputAvailableType", offered)?;
            // SAFETY: attribute read on the offered type.
            let subtype = unsafe { offered.GetGUID(&MF_MT_SUBTYPE) }.unwrap_or_default();
            if subtype != MFVideoFormat_NV12 {
                continue;
            }
            // SAFETY: sets an offered type on stream 0.
            let set = unsafe { self.mft.SetOutputType(0, &offered, 0) };
            if code_of(&set) == Some(MF_E_UNSUPPORTED_D3D_TYPE) {
                return Err(MfFail::D3dRefused);
            }
            call("SetOutputType", set)?;
            self.read_layout()?;
            return Ok(());
        }
        Err(MfFail::NoNv12)
    }

    fn read_layout(&mut self) -> Result<(), MfFail> {
        // SAFETY: reads of the current output type and stream info of stream 0.
        unsafe {
            let current = call("GetOutputCurrentType", self.mft.GetOutputCurrentType(0))?;
            let size = call("GetUINT64", current.GetUINT64(&MF_MT_FRAME_SIZE))?;
            let default_stride = current
                .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                .ok()
                .map(|value| i32::from_ne_bytes(value.to_ne_bytes()));
            let mut area = [0_u8; 16];
            let aperture = current
                .GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, &mut area, None)
                .ok()
                .map(|()| {
                    let at = |offset: usize| {
                        i32::from_le_bytes([
                            area[offset],
                            area[offset.saturating_add(1)],
                            area[offset.saturating_add(2)],
                            area[offset.saturating_add(3)],
                        ])
                    };
                    (at(8), at(12))
                });
            let info = call("GetOutputStreamInfo", self.mft.GetOutputStreamInfo(0))?;
            let provides = (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0)
                as u32;
            self.provides_samples = info.dwFlags & provides != 0;
            self.sample_bytes = info.cbSize;
            self.layout = OutLayout {
                width: u32::try_from(size >> 32).unwrap_or(0),
                height: u32::try_from(size & 0xFFFF_FFFF).unwrap_or(0),
                default_stride,
                aperture,
            };
        }
        Ok(())
    }

    /// Feeds one Annex B access unit with sample time `time`. `Ok(false)`: the decoder does not accept
    /// input now (`MF_E_NOTACCEPTING`); pull output, then retry.
    pub(crate) fn push(&self, bytes: &[u8], time: i64) -> Result<bool, MfFail> {
        let len = u32::try_from(bytes.len()).map_err(|_| MfFail::Call("input-bytes", 0))?;
        // SAFETY: a new memory buffer of `len` bytes is locked, filled with exactly `len` bytes and
        // unlocked before it is attached to a new sample; ProcessInput takes its own reference.
        unsafe {
            let buffer = call("MFCreateMemoryBuffer", MFCreateMemoryBuffer(len))?;
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut max = 0_u32;
            call("Lock", buffer.Lock(&mut data, Some(&mut max), None))?;
            if data.is_null() || max < len {
                let _ = buffer.Unlock();
                return Err(MfFail::Call("Lock-size", 0));
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
            call("Unlock", buffer.Unlock())?;
            call("SetCurrentLength", buffer.SetCurrentLength(len))?;
            let sample = call("MFCreateSample", MFCreateSample())?;
            call("AddBuffer", sample.AddBuffer(&buffer))?;
            call("SetSampleTime", sample.SetSampleTime(time))?;
            let result = self.mft.ProcessInput(0, &sample, 0);
            if code_of(&result) == Some(MF_E_NOTACCEPTING) {
                return Ok(false);
            }
            call("ProcessInput", result)?;
        }
        Ok(true)
    }

    /// End of stream: `NOTIFY_END_OF_STREAM` + `COMMAND_DRAIN`; pull afterwards until no more output.
    pub(crate) fn drain(&self) -> Result<(), MfFail> {
        // SAFETY: documented end-of-stream messages; no parameter.
        unsafe {
            call(
                "ProcessMessage",
                self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0),
            )?;
            call("ProcessMessage", self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0))?;
        }
        Ok(())
    }

    /// Pulls every picture the decoder can release now (until `MF_E_TRANSFORM_NEED_MORE_INPUT`). A stream
    /// change renegotiates NV12; `released_before` says whether this segment already released pictures, in
    /// which case a change is `FormatChange` (one format per segment).
    pub(crate) fn pull(
        &mut self,
        released_before: bool,
        out: &mut dyn FnMut(RawPicture) -> bool,
    ) -> Result<(), MfFail> {
        let mut released = released_before;
        loop {
            let provided = if self.provides_samples {
                None
            } else {
                Some(self.empty_sample()?)
            };
            let mut buffer = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(provided),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0_u32;
            // SAFETY: one output buffer for stream 0; afterwards both reference slots are taken back into
            // owned values (released on drop), whether the MFT filled them or not.
            let result = unsafe { self.mft.ProcessOutput(0, &mut buffer, &mut status) };
            let [slot] = buffer;
            let sample = ManuallyDrop::into_inner(slot.pSample);
            drop(ManuallyDrop::into_inner(slot.pEvents));
            match code_of(&result) {
                None => {}
                Some(code) if code == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                Some(code) if code == MF_E_TRANSFORM_STREAM_CHANGE => {
                    if released {
                        return Err(MfFail::FormatChange);
                    }
                    self.negotiate_nv12()?;
                    continue;
                }
                Some(code) => return Err(MfFail::Call("ProcessOutput", code.0)),
            }
            let Some(sample) = sample else {
                return Err(MfFail::Call("ProcessOutput-no-sample", 0));
            };
            let picture = self.read_sample(&sample)?;
            released = true;
            if !out(picture) {
                return Ok(());
            }
        }
    }

    fn empty_sample(&self) -> Result<IMFSample, MfFail> {
        // SAFETY: a new sample holding one new memory buffer of the size the stream info states.
        unsafe {
            let buffer = call("MFCreateMemoryBuffer", MFCreateMemoryBuffer(self.sample_bytes))?;
            let sample = call("MFCreateSample", MFCreateSample())?;
            call("AddBuffer", sample.AddBuffer(&buffer))?;
            Ok(sample)
        }
    }

    fn read_sample(&self, sample: &IMFSample) -> Result<RawPicture, MfFail> {
        // SAFETY: the contiguous buffer is locked, exactly `current` bytes are copied out, then unlocked.
        unsafe {
            let time = sample.GetSampleTime().unwrap_or(i64::MIN);
            let gpu_surface = sample
                .GetBufferByIndex(0)
                .is_ok_and(|first| first.cast::<IMFDXGIBuffer>().is_ok());
            let buffer = call("ConvertToContiguousBuffer", sample.ConvertToContiguousBuffer())?;
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut current = 0_u32;
            call("Lock", buffer.Lock(&mut data, None, Some(&mut current)))?;
            let bytes = if data.is_null() {
                Vec::new()
            } else {
                std::slice::from_raw_parts(data, usize::try_from(current).unwrap_or(0)).to_vec()
            };
            call("Unlock", buffer.Unlock())?;
            Ok(RawPicture {
                time,
                bytes,
                width: self.layout.width,
                height: self.layout.height,
                default_stride: self.layout.default_stride,
                aperture: self.layout.aperture,
                gpu_surface,
            })
        }
    }
}

// ---- D24-A: AAC decoder and video encoders (lane SMB-D24A-CODEC-DEFAULTS) -------------------------------

/// One output sample of a non-video transform: its time, clean-point flag and an owned copy of its bytes.
pub(crate) struct RawSample {
    pub time: i64,
    pub clean_point: bool,
    pub bytes: Vec<u8>,
}

/// A synchronous MFT on one stream: feed, drain, pull owned samples (shared by the AAC decoder and the
/// encoders; the H.264 decoder above keeps its own picture path).
struct Stream {
    mft: IMFTransform,
    provides_samples: bool,
    sample_bytes: u32,
}

impl Stream {
    fn new(mft: IMFTransform) -> Result<Self, MfFail> {
        let mut stream = Self {
            mft,
            provides_samples: false,
            sample_bytes: 0,
        };
        stream.read_info()?;
        // SAFETY: documented start-up messages of the basic processing model; no parameter.
        unsafe {
            call(
                "ProcessMessage",
                stream.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0),
            )?;
            call(
                "ProcessMessage",
                stream.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0),
            )?;
        }
        Ok(stream)
    }

    fn read_info(&mut self) -> Result<(), MfFail> {
        // SAFETY: stream info read of stream 0.
        let info = call("GetOutputStreamInfo", unsafe { self.mft.GetOutputStreamInfo(0) })?;
        let provides =
            (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32;
        self.provides_samples = info.dwFlags & provides != 0;
        self.sample_bytes = info.cbSize.max(1);
        Ok(())
    }

    /// `Ok(false)`: `MF_E_NOTACCEPTING` (pull, then retry).
    fn push(&self, bytes: &[u8], time: i64, duration: i64) -> Result<bool, MfFail> {
        let len = u32::try_from(bytes.len()).map_err(|_| MfFail::Call("input-bytes", 0))?;
        // SAFETY: as `Decoder::push`: a new buffer is locked, filled with exactly `len` bytes, unlocked and
        // attached to a new sample; ProcessInput takes its own reference.
        unsafe {
            let buffer = call("MFCreateMemoryBuffer", MFCreateMemoryBuffer(len.max(1)))?;
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut max = 0_u32;
            call("Lock", buffer.Lock(&mut data, Some(&mut max), None))?;
            if data.is_null() || max < len {
                let _ = buffer.Unlock();
                return Err(MfFail::Call("Lock-size", 0));
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
            call("Unlock", buffer.Unlock())?;
            call("SetCurrentLength", buffer.SetCurrentLength(len))?;
            let sample = call("MFCreateSample", MFCreateSample())?;
            call("AddBuffer", sample.AddBuffer(&buffer))?;
            call("SetSampleTime", sample.SetSampleTime(time))?;
            call("SetSampleDuration", sample.SetSampleDuration(duration))?;
            let result = self.mft.ProcessInput(0, &sample, 0);
            if code_of(&result) == Some(MF_E_NOTACCEPTING) {
                return Ok(false);
            }
            call("ProcessInput", result)?;
        }
        Ok(true)
    }

    fn drain(&self) -> Result<(), MfFail> {
        // SAFETY: documented end-of-stream messages; no parameter.
        unsafe {
            call(
                "ProcessMessage",
                self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0),
            )?;
            call(
                "ProcessMessage",
                self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0),
            )?;
        }
        Ok(())
    }

    /// Pulls every sample the transform releases now (until `MF_E_TRANSFORM_NEED_MORE_INPUT`); more than
    /// `max` samples is `Call("too-many-outputs")`. A stream change calls `renegotiate`.
    fn pull(
        &mut self,
        max: usize,
        renegotiate: &mut dyn FnMut(&IMFTransform) -> Result<(), MfFail>,
    ) -> Result<Vec<RawSample>, MfFail> {
        let mut out = Vec::new();
        loop {
            let provided = if self.provides_samples {
                None
            } else {
                // SAFETY: a new sample holding one new memory buffer of the size the stream info states.
                Some(unsafe {
                    let buffer =
                        call("MFCreateMemoryBuffer", MFCreateMemoryBuffer(self.sample_bytes))?;
                    let sample = call("MFCreateSample", MFCreateSample())?;
                    call("AddBuffer", sample.AddBuffer(&buffer))?;
                    sample
                })
            };
            let mut buffer = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(provided),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0_u32;
            // SAFETY: one output buffer for stream 0; both reference slots are taken back afterwards.
            let result = unsafe { self.mft.ProcessOutput(0, &mut buffer, &mut status) };
            let [slot] = buffer;
            let sample = ManuallyDrop::into_inner(slot.pSample);
            drop(ManuallyDrop::into_inner(slot.pEvents));
            match code_of(&result) {
                None => {}
                Some(code) if code == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(out),
                Some(code) if code == MF_E_TRANSFORM_STREAM_CHANGE => {
                    renegotiate(&self.mft)?;
                    self.read_info()?;
                    continue;
                }
                Some(code) => return Err(MfFail::Call("ProcessOutput", code.0)),
            }
            let Some(sample) = sample else {
                return Err(MfFail::Call("ProcessOutput-no-sample", 0));
            };
            out.push(read_raw(&sample)?);
            if out.len() > max {
                return Err(MfFail::Call("too-many-outputs", 0));
            }
        }
    }
}

fn read_raw(sample: &IMFSample) -> Result<RawSample, MfFail> {
    // SAFETY: the contiguous buffer is locked, exactly `current` bytes are copied out, then unlocked.
    unsafe {
        let time = sample.GetSampleTime().unwrap_or(i64::MIN);
        let clean_point = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
        let buffer = call(
            "ConvertToContiguousBuffer",
            sample.ConvertToContiguousBuffer(),
        )?;
        let mut data: *mut u8 = std::ptr::null_mut();
        let mut current = 0_u32;
        call("Lock", buffer.Lock(&mut data, None, Some(&mut current)))?;
        let bytes = if data.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(data, usize::try_from(current).unwrap_or(0)).to_vec()
        };
        call("Unlock", buffer.Unlock())?;
        Ok(RawSample {
            time,
            clean_point,
            bytes,
        })
    }
}

/// Decoded PCM layout the AAC decoder delivers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PcmOut {
    pub float: bool,
    pub channels: u32,
    pub rate: u32,
    /// `MF_MT_AUDIO_CHANNEL_MASK` of the output type (0 when not set).
    pub mask: u32,
}

/// Picks the output type: 32-bit float of the stream's channel count, else 16-bit PCM of it.
fn negotiate_pcm(mft: &IMFTransform, channels: u32) -> Result<PcmOut, MfFail> {
    let mut chosen: Option<(bool, IMFMediaType)> = None;
    for index in 0..64_u32 {
        // SAFETY: enumeration of the output types of stream 0.
        let offered = unsafe { mft.GetOutputAvailableType(0, index) };
        if code_of(&offered) == Some(MF_E_NO_MORE_TYPES) {
            break;
        }
        let offered = call("GetOutputAvailableType", offered)?;
        // SAFETY: attribute reads on the offered type.
        let (subtype, count, bits) = unsafe {
            (
                offered.GetGUID(&MF_MT_SUBTYPE).unwrap_or_default(),
                offered.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).unwrap_or(0),
                offered.GetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE).unwrap_or(0),
            )
        };
        if count != channels {
            continue;
        }
        if subtype == MFAudioFormat_Float && bits == 32 {
            chosen = Some((true, offered));
            break;
        }
        if subtype == MFAudioFormat_PCM && bits == 16 && chosen.is_none() {
            chosen = Some((false, offered));
        }
    }
    let Some((float, chosen)) = chosen else {
        return Err(MfFail::NoNv12);
    };
    // SAFETY: sets an offered type on stream 0, then reads the current type.
    unsafe {
        call("SetOutputType", mft.SetOutputType(0, &chosen, 0))?;
        let current = call("GetOutputCurrentType", mft.GetOutputCurrentType(0))?;
        Ok(PcmOut {
            float,
            channels: call("GetUINT32", current.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS))?,
            rate: call(
                "GetUINT32",
                current.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND),
            )?,
            mask: current.GetUINT32(&MF_MT_AUDIO_CHANNEL_MASK).unwrap_or(0),
        })
    }
}

/// The OS AAC decoder MFT for one stream (raw access units in, PCM out).
pub(crate) struct AacDecoder {
    stream: Stream,
    pub out: PcmOut,
}

impl AacDecoder {
    /// Activates `entry` with input `{Audio, AAC}` raw payload (Learn "AAC Decoder": `MF_MT_USER_DATA` is the
    /// HEAACWAVEINFO tail after the WAVEFORMATEX, wPayloadType 0 = raw, then the AudioSpecificConfig) and a
    /// float (else 16-bit) PCM output type of the same channel count; `NoNv12` when it offers neither.
    pub(crate) fn open(
        entry: &DecoderEntry,
        rate: u32,
        channels: u32,
        asc: &[u8],
    ) -> Result<Self, MfFail> {
        // SAFETY: activation of an object MFTEnumEx returned; the result is an owned interface.
        let mft: IMFTransform = call("ActivateObject", unsafe { entry.activate.ActivateObject() })?;
        let mut user = Vec::with_capacity(12_usize.saturating_add(asc.len()));
        user.extend_from_slice(&0_u16.to_le_bytes()); // wPayloadType: raw access units
        user.extend_from_slice(&0xFE_u16.to_le_bytes()); // wAudioProfileLevelIndication: unknown
        user.extend_from_slice(&0_u16.to_le_bytes()); // wStructType
        user.extend_from_slice(&0_u16.to_le_bytes()); // wReserved1
        user.extend_from_slice(&0_u32.to_le_bytes()); // dwReserved2
        user.extend_from_slice(asc);
        // SAFETY: plain COM object creation and attribute writes on the new type; stream 0 of the MFT.
        unsafe {
            let input = call("MFCreateMediaType", MFCreateMediaType())?;
            call("SetGUID", input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio))?;
            call("SetGUID", input.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC))?;
            call("SetUINT32", input.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate))?;
            call("SetUINT32", input.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels))?;
            call("SetUINT32", input.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16))?;
            call("SetUINT32", input.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0))?;
            call(
                "SetUINT32",
                input.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0xFE),
            )?;
            call("SetBlob", input.SetBlob(&MF_MT_USER_DATA, &user))?;
            call("SetInputType", mft.SetInputType(0, &input, 0))?;
        }
        let out = negotiate_pcm(&mft, channels)?;
        Ok(Self {
            stream: Stream::new(mft)?,
            out,
        })
    }

    pub(crate) fn push(&self, bytes: &[u8], time: i64, duration: i64) -> Result<bool, MfFail> {
        self.stream.push(bytes, time, duration)
    }

    pub(crate) fn drain(&self) -> Result<(), MfFail> {
        self.stream.drain()
    }

    /// Every PCM sample the decoder releases now; a format change is `FormatChange`.
    pub(crate) fn pull(&mut self, max: usize) -> Result<Vec<RawSample>, MfFail> {
        let expected = self.out;
        self.stream.pull(max, &mut |mft| {
            if negotiate_pcm(mft, expected.channels)? == expected {
                Ok(())
            } else {
                Err(MfFail::FormatChange)
            }
        })
    }
}

/// What an encoder session is opened for.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(feature = "os-encode"), allow(dead_code))]
pub(crate) struct EncodeSetup {
    pub hevc: bool,
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub bitrate: u32,
    /// `MF_MT_MPEG2_PROFILE` (H.264 eAVEncH264VProfile 66/77/100; HEVC eAVEncH265VProfile_Main_420_8 = 1).
    pub profile: u32,
}

/// One OS video encoder MFT (NV12 in, Annex B out).
#[cfg_attr(not(feature = "os-encode"), allow(dead_code))]
pub(crate) struct Encoder {
    stream: Stream,
}

#[cfg_attr(not(feature = "os-encode"), allow(dead_code))]
impl Encoder {
    /// Activates `entry`; output type first (encoders need it before the input type, Learn "H.264 Video
    /// Encoder"): `{Video, H264|HEVC}`, frame size, frame rate, average bitrate, progressive, square
    /// pixels, profile; then input NV12 of the same size and rate.
    pub(crate) fn open(entry: &DecoderEntry, setup: &EncodeSetup) -> Result<Self, MfFail> {
        // SAFETY: activation of an object MFTEnumEx returned; the result is an owned interface.
        let mft: IMFTransform = call("ActivateObject", unsafe { entry.activate.ActivateObject() })?;
        let subtype = if setup.hevc {
            MFVideoFormat_HEVC
        } else {
            MFVideoFormat_H264
        };
        let size = (u64::from(setup.width) << 32) | u64::from(setup.height);
        let rate = (u64::from(setup.fps_num) << 32) | u64::from(setup.fps_den);
        let progressive = u32::try_from(MFVideoInterlace_Progressive.0).unwrap_or(2);
        // SAFETY: plain COM object creation and attribute writes on the new types; stream 0 of the MFT.
        unsafe {
            let output = call("MFCreateMediaType", MFCreateMediaType())?;
            call("SetGUID", output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video))?;
            call("SetGUID", output.SetGUID(&MF_MT_SUBTYPE, &subtype))?;
            call("SetUINT64", output.SetUINT64(&MF_MT_FRAME_SIZE, size))?;
            call("SetUINT64", output.SetUINT64(&MF_MT_FRAME_RATE, rate))?;
            call(
                "SetUINT64",
                output.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1_u64 << 32) | 1),
            )?;
            call("SetUINT32", output.SetUINT32(&MF_MT_AVG_BITRATE, setup.bitrate))?;
            call("SetUINT32", output.SetUINT32(&MF_MT_INTERLACE_MODE, progressive))?;
            call("SetUINT32", output.SetUINT32(&MF_MT_MPEG2_PROFILE, setup.profile))?;
            call("SetOutputType", mft.SetOutputType(0, &output, 0))?;
            let input = call("MFCreateMediaType", MFCreateMediaType())?;
            call("SetGUID", input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video))?;
            call("SetGUID", input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12))?;
            call("SetUINT64", input.SetUINT64(&MF_MT_FRAME_SIZE, size))?;
            call("SetUINT64", input.SetUINT64(&MF_MT_FRAME_RATE, rate))?;
            call(
                "SetUINT64",
                input.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1_u64 << 32) | 1),
            )?;
            call("SetUINT32", input.SetUINT32(&MF_MT_INTERLACE_MODE, progressive))?;
            call("SetInputType", mft.SetInputType(0, &input, 0))?;
        }
        Ok(Self {
            stream: Stream::new(mft)?,
        })
    }

    pub(crate) fn push(&self, nv12: &[u8], time: i64, duration: i64) -> Result<bool, MfFail> {
        self.stream.push(nv12, time, duration)
    }

    pub(crate) fn drain(&self) -> Result<(), MfFail> {
        self.stream.drain()
    }

    /// Every coded unit the encoder releases now (a stream change after start is refused).
    pub(crate) fn pull(&mut self, max: usize) -> Result<Vec<RawSample>, MfFail> {
        self.stream.pull(max, &mut |_| Err(MfFail::FormatChange))
    }
}
