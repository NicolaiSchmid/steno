//! The COM and WASAPI calls behind the Windows capture backend and the
//! audio-session enumeration: the Windows backend's only `unsafe` (the
//! crate-wide list is in the crate doc). Written against Microsoft's
//! documentation and tested on the `windows-latest` CI runner, which has
//! no audio endpoint; no Windows machine with audio devices has run it
//! (see the module doc of `capture::live::wasapi`). WP10a of
//! `.plans/2026-10-02-rust-core-and-tauri-shell.md`; the macOS counterpart
//! is `capture::live::hal`. No Swift counterpart.
//!
//! # Invariants every `unsafe` block here relies on
//!
//! - **Apartment.** Every COM call happens on a thread holding an
//!   [`Apartment`] (`CoInitializeEx`, multithreaded), and every interface
//!   obtained on a thread is used and released on that same thread: the
//!   wrappers hold `windows` interfaces, which are neither `Send` nor
//!   `Sync`, so the compiler keeps them there. The apartment guard is
//!   declared before the objects that need it, so it is dropped (and
//!   `CoUninitialize` runs) after them.
//! - **Reference counting.** `windows` interfaces release themselves on
//!   drop; nothing here calls `AddRef` or `Release` by hand.
//! - **Callee-allocated memory.** Strings and formats WASAPI returns
//!   (`GetId`, `GetMixFormat`) are copied and freed with `CoTaskMemFree` in
//!   the function that received them; nothing keeps such a pointer. A
//!   `PROPVARIANT` frees its contents in `Drop` (`PropVariantClear`), so a
//!   variant the store filled is simply dropped, and one that points at our
//!   memory (the activation blob) is `ManuallyDrop` and never cleared; it
//!   and the blob stay on the heap until the activation has completed.
//! - **Capture buffers.** A packet from `IAudioCaptureClient::GetBuffer`
//!   is lent to the caller's closure as a slice and released with
//!   `ReleaseBuffer` right after the closure returns; the slice cannot
//!   escape the closure (its lifetime is the call's).
//! - **Callbacks.** The notification objects (`#[implement]`) run on
//!   threads the audio service owns. They only call the boxed closure they
//!   were given, which sends on a channel or records the time under the
//!   capture watcher's short lock (never held across a WASAPI call); they
//!   never call back into WASAPI (the documentation forbids it from inside
//!   `IMMNotificationClient`) and never wait on anything else. Each
//!   registration is undone in `Drop`. Microsoft does not say whether a
//!   callback can still be running when the unregister call returns; it
//!   does not matter for memory safety, because the object is reference
//!   counted and its closure owns everything it touches (an `Arc` or a
//!   `Sender`).

// `#[implement]` expands to `&T as *const T` casts.
#![allow(clippy::ref_as_ptr)]

use std::ffi::c_void;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ptr::NonNull;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::Duration;

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{CloseHandle, HANDLE, PROPERTYKEY, RPC_E_CHANGED_MODE, S_OK};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, AUDIOCLIENT_ACTIVATION_PARAMS,
    AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
    AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS, ActivateAudioInterfaceAsync, AudioSessionDisconnectReason,
    AudioSessionState, AudioSessionStateActive, AudioSessionStateExpired, DEVICE_STATE,
    DEVICE_STATE_ACTIVE, EDataFlow, ERole, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioCaptureClient, IAudioClient, IAudioSessionControl, IAudioSessionControl2,
    IAudioSessionEvents, IAudioSessionEvents_Impl, IAudioSessionManager2,
    IAudioSessionNotification, IAudioSessionNotification_Impl, IMMDevice, IMMDeviceEnumerator,
    IMMNotificationClient, IMMNotificationClient_Impl, MMDeviceEnumerator,
    PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
    WAVEFORMATEX, eCapture, eConsole, eRender,
};
use windows::Win32::Media::Multimedia::WAVE_FORMAT_IEEE_FLOAT;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{
    BLOB, CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize, IAgileObject, IAgileObject_Impl, STGM_READ,
};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW, OpenProcess,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    WaitForSingleObject,
};
use windows::Win32::System::Variant::{VT_BLOB, VT_LPWSTR};
use windows::core::{BOOL, GUID, HRESULT, HSTRING, IUnknown, Interface, PCWSTR, PWSTR, Ref, w};
use windows_core::implement;

use crate::SAMPLE_RATE;
use crate::capture::CaptureError;
use crate::capture::split_streams::{BUFFER_DURATION, StreamSizes, stream_sizes};
use crate::detection::{AudioSessionRecord, EndpointFlow, SessionState};
use crate::realtime::SliceView;

/// A failed COM or WASAPI call: which one, its `HRESULT`, the system's
/// message for it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{operation} failed: {code:#010x} ({message})")]
pub struct ComError {
    /// The interface method or function.
    pub operation: &'static str,
    /// The `HRESULT`'s bits.
    pub code: u32,
    /// The system's text for it.
    pub message: String,
}

impl ComError {
    fn new(operation: &'static str, error: &windows::core::Error) -> Self {
        Self {
            operation,
            // The HRESULT's bit pattern, printed in hex.
            code: error.code().0 as u32,
            message: error.message(),
        }
    }

    /// A failure with no `HRESULT` behind it (code 0).
    fn other(operation: &'static str, message: impl Into<String>) -> Self {
        Self {
            operation,
            code: 0,
            message: message.into(),
        }
    }

    /// The stream's device went away or was reconfigured
    /// (`AUDCLNT_E_DEVICE_INVALIDATED`): rebuild, do not retry.
    #[must_use]
    pub fn is_device_invalidated(&self) -> bool {
        self.code == AUDCLNT_E_DEVICE_INVALIDATED.0 as u32
    }
}

impl From<ComError> for CaptureError {
    fn from(error: ComError) -> Self {
        CaptureError::BackendFailed(error.to_string())
    }
}

/// `result` with the failing operation's name.
fn check<T>(result: windows::core::Result<T>, operation: &'static str) -> Result<T, ComError> {
    result.map_err(|error| ComError::new(operation, &error))
}

/// COM on the current thread, multithreaded apartment, for as long as the
/// guard lives. A thread already in a single-threaded apartment (a UI
/// thread) keeps it: COM is usable there too, and the guard then leaves
/// the uninitialisation to whoever initialised it.
pub struct Apartment {
    uninitialize: bool,
    /// Thread-bound: `CoUninitialize` must run on the initialising thread.
    _thread: PhantomData<*const ()>,
}

impl Apartment {
    /// Joins the multithreaded apartment.
    pub fn enter() -> Result<Self, ComError> {
        // SAFETY: no pointer is passed (the reserved argument is null);
        // the call affects only the calling thread, and a success is
        // balanced by `CoUninitialize` in `Drop` on this same thread
        // (the guard is `!Send`).
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let uninitialize = result != RPC_E_CHANGED_MODE;
        if uninitialize {
            check(result.ok(), "CoInitializeEx")?;
        }
        Ok(Self {
            uninitialize,
            _thread: PhantomData,
        })
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.uninitialize {
            // SAFETY: balances this thread's successful `CoInitializeEx`;
            // every interface created under it was dropped first (the
            // guard is declared before them, see the module doc).
            unsafe { CoUninitialize() };
        }
    }
}

/// Copies a string WASAPI allocated with the COM allocator and frees it.
///
/// # Safety
///
/// `value` is null or a NUL-terminated UTF-16 string allocated with
/// `CoTaskMemAlloc` that the caller owns and does not use afterwards.
unsafe fn take_co_string(value: PWSTR) -> Option<String> {
    if value.is_null() {
        return None;
    }
    // SAFETY: by the caller's guarantee `value` is a live NUL-terminated
    // string; it is copied before it is freed, exactly once.
    unsafe {
        let text = value.to_string().ok();
        CoTaskMemFree(Some(value.as_ptr().cast_const().cast::<c_void>()));
        text
    }
}

fn data_flow(flow: EndpointFlow) -> EDataFlow {
    match flow {
        EndpointFlow::Capture => eCapture,
        EndpointFlow::Render => eRender,
    }
}

/// `IMMDeviceEnumerator`: the default endpoints, endpoints by id and by
/// flow, and the endpoint notifications.
pub struct Enumerator(IMMDeviceEnumerator);

impl Enumerator {
    /// Needs an [`Apartment`] on this thread.
    pub fn new() -> Result<Self, ComError> {
        // SAFETY: a plain activation of the documented MMDevice class on
        // a thread with COM initialised (module invariant); the returned
        // interface is reference counted by `windows`.
        let enumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) };
        check(enumerator, "CoCreateInstance(MMDeviceEnumerator)").map(Self)
    }

    /// The default endpoint for `flow` in the `eConsole` role, the one
    /// most audio plays on and records from. (`eCommunications`, the call
    /// apps' default, is not followed: no stream here opens it.)
    pub fn default_endpoint(&self, flow: EndpointFlow) -> Result<Endpoint, ComError> {
        // SAFETY: plain values in, a counted interface out.
        let device = unsafe { self.0.GetDefaultAudioEndpoint(data_flow(flow), eConsole) };
        check(device, "IMMDeviceEnumerator::GetDefaultAudioEndpoint").map(Endpoint)
    }

    /// The endpoint with this id (`IMMDevice::GetId`).
    pub fn endpoint(&self, id: &str) -> Result<Endpoint, ComError> {
        let id = HSTRING::from(id);
        // SAFETY: `id` is a NUL-terminated wide string alive for the call.
        let device = unsafe { self.0.GetDevice(&id) };
        check(device, "IMMDeviceEnumerator::GetDevice").map(Endpoint)
    }

    /// Every active endpoint for `flow`.
    pub fn active_endpoints(&self, flow: EndpointFlow) -> Result<Vec<Endpoint>, ComError> {
        // SAFETY: plain values in, a counted collection out.
        let collection = check(
            unsafe {
                self.0
                    .EnumAudioEndpoints(data_flow(flow), DEVICE_STATE_ACTIVE)
            },
            "IMMDeviceEnumerator::EnumAudioEndpoints",
        )?;
        // SAFETY: a plain call on the live collection.
        let count = check(
            unsafe { collection.GetCount() },
            "IMMDeviceCollection::GetCount",
        )?;
        Ok((0..count)
            // SAFETY: `index` is below the count the collection reported.
            .filter_map(|index| unsafe { collection.Item(index) }.ok().map(Endpoint))
            .collect())
    }

    /// Calls `on_event` for every endpoint notification (a default
    /// changed, a device added, removed or changing state) until the
    /// registration is dropped. Which device and role is not passed on:
    /// the listener resolves the devices again. `on_event` runs on a
    /// thread of the audio service and must neither block nor call into
    /// WASAPI.
    pub fn register(
        &self,
        on_event: Box<dyn Fn() + Send + Sync>,
    ) -> Result<EndpointRegistration, ComError> {
        let client: IMMNotificationClient = EndpointNotifications { on_event }.into();
        check(
            // SAFETY: `client` is a live COM object; it stays referenced by
            // the registration until `UnregisterEndpointNotificationCallback`
            // in `Drop`.
            unsafe { self.0.RegisterEndpointNotificationCallback(&client) },
            "IMMDeviceEnumerator::RegisterEndpointNotificationCallback",
        )?;
        Ok(EndpointRegistration {
            enumerator: self.0.clone(),
            client,
        })
    }
}

#[implement(IMMNotificationClient)]
struct EndpointNotifications {
    on_event: Box<dyn Fn() + Send + Sync>,
}

impl IMMNotificationClient_Impl for EndpointNotifications_Impl {
    fn OnDeviceStateChanged(
        &self,
        _id: &PCWSTR,
        _state: DEVICE_STATE,
    ) -> windows::core::Result<()> {
        (self.on_event)();
        Ok(())
    }

    fn OnDeviceAdded(&self, _id: &PCWSTR) -> windows::core::Result<()> {
        (self.on_event)();
        Ok(())
    }

    fn OnDeviceRemoved(&self, _id: &PCWSTR) -> windows::core::Result<()> {
        (self.on_event)();
        Ok(())
    }

    fn OnDefaultDeviceChanged(
        &self,
        _flow: EDataFlow,
        _role: ERole,
        _id: &PCWSTR,
    ) -> windows::core::Result<()> {
        (self.on_event)();
        Ok(())
    }

    fn OnPropertyValueChanged(
        &self,
        _id: &PCWSTR,
        _key: &PROPERTYKEY,
    ) -> windows::core::Result<()> {
        // Fires for every volume and format property; a format change
        // invalidates the stream instead, which the capture thread sees.
        Ok(())
    }
}

/// Unregisters the endpoint notifications on drop.
pub struct EndpointRegistration {
    enumerator: IMMDeviceEnumerator,
    client: IMMNotificationClient,
}

impl Drop for EndpointRegistration {
    fn drop(&mut self) {
        // SAFETY: undoes the registration made with this same client on
        // this same enumerator. A callback still in flight only touches the
        // client, which it holds a reference to (module doc).
        let _ = unsafe {
            self.enumerator
                .UnregisterEndpointNotificationCallback(&self.client)
        };
    }
}

/// `IMMDevice`: one endpoint.
pub struct Endpoint(IMMDevice);

impl Endpoint {
    /// The endpoint id, stable across reboots: what `Settings` stores as
    /// the input device UID.
    pub fn id(&self) -> Result<String, ComError> {
        // SAFETY: on success the string is ours to free, which
        // `take_co_string` does after copying it.
        let id = check(unsafe { self.0.GetId() }, "IMMDevice::GetId")?;
        // SAFETY: as above.
        unsafe { take_co_string(id) }
            .ok_or_else(|| ComError::other("IMMDevice::GetId", "empty endpoint id"))
    }

    /// `DEVICE_STATE_ACTIVE`: present, enabled, not unplugged.
    #[must_use]
    pub fn is_active(&self) -> bool {
        // SAFETY: a plain call on the live device.
        unsafe { self.0.GetState() }.is_ok_and(|state| state == DEVICE_STATE_ACTIVE)
    }

    /// `PKEY_Device_FriendlyName` ("Speakers (Realtek(R) Audio)").
    #[must_use]
    pub fn friendly_name(&self) -> Option<String> {
        // SAFETY: a plain call on the live device; the store is counted.
        let store = unsafe { self.0.OpenPropertyStore(STGM_READ) }.ok()?;
        let key = PKEY_Device_FriendlyName;
        // SAFETY: the key is a live local for the call; on success the
        // variant is ours, and its `Drop` (`PropVariantClear`) frees what
        // the store allocated.
        let value: PROPVARIANT = unsafe { store.GetValue(&raw const key) }.ok()?;
        // SAFETY: `vt` says which union arm is live; `pwszVal` is read
        // only when it is `VT_LPWSTR`, and copied before `value` drops.
        unsafe {
            let inner = &value.Anonymous.Anonymous;
            if inner.vt == VT_LPWSTR {
                inner.Anonymous.pwszVal.to_string().ok()
            } else {
                None
            }
        }
    }

    /// The shared-mode mix format's channels and rate.
    #[must_use]
    pub fn mix_format(&self) -> Option<(usize, u32)> {
        let client = self.audio_client().ok()?;
        // SAFETY: on success the format is ours to free; it is read by
        // value (the struct is packed, so unaligned) before the free.
        unsafe {
            let format = client.GetMixFormat().ok()?;
            if format.is_null() {
                return None;
            }
            let value = format.read_unaligned();
            CoTaskMemFree(Some(format.cast_const().cast::<c_void>()));
            Some((usize::from(value.nChannels), value.nSamplesPerSec))
        }
    }

    fn audio_client(&self) -> Result<IAudioClient, ComError> {
        check(
            // SAFETY: activation of a documented interface with no parameters.
            unsafe { self.0.Activate::<IAudioClient>(CLSCTX_ALL, None) },
            "IMMDevice::Activate(IAudioClient)",
        )
    }

    /// The endpoint's session manager.
    pub fn session_manager(&self) -> Result<SessionManager, ComError> {
        check(
            // SAFETY: activation of a documented interface with no parameters.
            unsafe { self.0.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) },
            "IMMDevice::Activate(IAudioSessionManager2)",
        )
        .map(SessionManager)
    }
}

/// 32-bit float, interleaved, 48 kHz, `channels` channels.
fn float_format(channels: usize) -> WAVEFORMATEX {
    // At most two channels are ever asked for.
    let channels = channels as u16;
    let block_align = channels * 4;
    let rate = SAMPLE_RATE as u32;
    WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_IEEE_FLOAT as u16,
        nChannels: channels,
        nSamplesPerSec: rate,
        nAvgBytesPerSec: rate * u32::from(block_align),
        nBlockAlign: block_align,
        wBitsPerSample: 32,
        cbSize: 0,
    }
}

/// How often a polled stream (no event) is drained: 50 ms, half its buffer.
pub const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// [`POLL_INTERVAL`] as a `REFERENCE_TIME`.
const POLL_INTERVAL_HUNDRED_NANOSECONDS: i64 = (POLL_INTERVAL.as_nanos() / 100) as i64;

/// A Win32 auto-reset event the engine signals per period.
struct Event(HANDLE);

impl Event {
    fn new() -> Result<Self, ComError> {
        check(
            // SAFETY: no security attributes, no name; the handle is closed in
            // `Drop`.
            unsafe { CreateEventW(None, false, false, PCWSTR::null()) },
            "CreateEventW",
        )
        .map(Self)
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: the handle came from `CreateEventW` and is closed once;
        // the client that signalled it was stopped (and is released
        // before this field, by declaration order in `CaptureClient`).
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Which loopback a system stream ended up on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopbackKind {
    /// Process loopback excluding Steno's own process tree. Microsoft's
    /// API page names build 20438 and its ApplicationLoopback sample build
    /// 20348; it is reported to work from Windows 10 2004 (build 19041).
    /// Unverified here; the endpoint fallback covers either.
    Process,
    /// Loopback of the default render endpoint, Steno's own output
    /// included.
    Endpoint,
}

/// One initialised shared-mode capture stream: the client, its capture
/// service, the event it signals.
pub struct CaptureClient {
    capture: IAudioCaptureClient,
    client: IAudioClient,
    /// `None` when the stream polls (endpoint loopback where the engine
    /// refused event callbacks).
    event: Option<Event>,
    channels: usize,
    sizes: StreamSizes,
    discontinuities: u64,
    /// No packet drained yet: loopback streams commonly flag their first
    /// packet as a discontinuity, which is not a late thread.
    first_packet: bool,
}

impl CaptureClient {
    /// The microphone stream on `endpoint`, `channels` channels at 48 kHz
    /// float, converted by the engine from the device's own format.
    pub fn microphone(endpoint: &Endpoint, channels: usize) -> Result<Self, ComError> {
        let flags = AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        Self::initialize(endpoint.audio_client()?, flags, channels, true)
    }

    /// Loopback of the render `endpoint`. Event-driven where the engine
    /// supports it for loopback (Windows 10 1703 and later), polled
    /// otherwise.
    pub fn endpoint_loopback(endpoint: &Endpoint, channels: usize) -> Result<Self, ComError> {
        let flags = AUDCLNT_STREAMFLAGS_LOOPBACK
            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        match Self::initialize(endpoint.audio_client()?, flags, channels, true) {
            Ok(client) => Ok(client),
            Err(error) => {
                tracing::info!("event-driven endpoint loopback refused ({error}); polling");
                Self::initialize(endpoint.audio_client()?, flags, channels, false)
            }
        }
    }

    /// Process loopback of everything but `excluded_pid`'s process tree,
    /// waiting at most `timeout` for the asynchronous activation.
    pub fn process_loopback(
        excluded_pid: u32,
        channels: usize,
        timeout: Duration,
    ) -> Result<Self, ComError> {
        let client = activate_process_loopback(excluded_pid, timeout)?;
        // The flags of Microsoft's ApplicationLoopback sample; the virtual
        // device converts to the format asked for.
        let flags = AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM;
        Self::initialize(client, flags, channels, true)
    }

    fn initialize(
        client: IAudioClient,
        flags: u32,
        channels: usize,
        event_driven: bool,
    ) -> Result<Self, ComError> {
        let format = float_format(channels);
        let flags = if event_driven {
            flags | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
        } else {
            flags
        };
        check(
            // SAFETY: `format` is a complete WAVEFORMATEX (`cbSize` 0) alive
            // for the call, which copies it; no session GUID.
            unsafe {
                client.Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    flags,
                    BUFFER_DURATION,
                    0,
                    &raw const format,
                    None,
                )
            },
            "IAudioClient::Initialize",
        )?;
        let event = if event_driven {
            let event = Event::new()?;
            check(
                // SAFETY: the event outlives the client's use of it: it is a
                // field dropped after `client` (declaration order) and after
                // `stop`.
                unsafe { client.SetEventHandle(event.0) },
                "IAudioClient::SetEventHandle",
            )?;
            Some(event)
        } else {
            None
        };
        // SAFETY: plain calls on the initialised client.
        let buffer_frames = check(
            unsafe { client.GetBufferSize() },
            "IAudioClient::GetBufferSize",
        )? as usize;
        // SAFETY: as above; the service is a counted interface.
        let capture: IAudioCaptureClient = check(
            unsafe { client.GetService() },
            "IAudioClient::GetService(IAudioCaptureClient)",
        )?;
        // SAFETY: plain call on the initialised client.
        let latency = unsafe { client.GetStreamLatency() }.ok();
        let period = if event.is_none() {
            // A polled stream is drained once per poll, so that is its
            // packet rhythm, whatever the engine's period.
            Some(POLL_INTERVAL_HUNDRED_NANOSECONDS)
        } else {
            let mut period: i64 = 0;
            // SAFETY: `period` is a live i64 for the call to write.
            let read = unsafe { client.GetDevicePeriod(Some(&raw mut period), None) };
            read.is_ok().then_some(period)
        };
        Ok(Self {
            capture,
            client,
            event,
            channels,
            sizes: stream_sizes(buffer_frames, period, latency),
            discontinuities: 0,
            first_packet: true,
        })
    }

    /// The buffer, period and latency the stream runs with: what the
    /// engine answered, where [`stream_sizes`] trusts it.
    #[must_use]
    pub fn sizes(&self) -> StreamSizes {
        self.sizes
    }

    /// Packets flagged `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY` so far, the
    /// first packet aside: the engine lost data because the thread was
    /// late.
    #[must_use]
    pub fn discontinuities(&self) -> u64 {
        self.discontinuities
    }

    /// Starts the stream.
    pub fn start(&self) -> Result<(), ComError> {
        // SAFETY: plain call on the initialised client.
        check(unsafe { self.client.Start() }, "IAudioClient::Start")
    }

    /// Stops the stream; the engine stops signalling the event.
    pub fn stop(&self) -> Result<(), ComError> {
        // SAFETY: plain call on the initialised client.
        check(unsafe { self.client.Stop() }, "IAudioClient::Stop")
    }

    /// Waits for the engine's signal, at most `timeout` (a polled stream
    /// sleeps [`POLL_INTERVAL`] instead). The caller drains either way, so
    /// a timeout is not reported.
    pub fn wait(&self, timeout: Duration) {
        let Some(event) = &self.event else {
            std::thread::sleep(POLL_INTERVAL);
            return;
        };
        let milliseconds = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: the handle is a live event owned by `self`.
        let _ = unsafe { WaitForSingleObject(event.0, milliseconds) };
    }

    /// Hands every queued packet to `handle`, then returns. The real-time
    /// part: no allocation and no lock here on success (the `windows`
    /// error values are built only on failure); `handle` must keep it so.
    pub fn drain(&mut self, mut handle: impl FnMut(SliceView<'_>)) -> Result<(), ComError> {
        loop {
            // SAFETY: plain call on the started capture service.
            let next = check(
                unsafe { self.capture.GetNextPacketSize() },
                "IAudioCaptureClient::GetNextPacketSize",
            )?;
            if next == 0 {
                return Ok(());
            }
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames: u32 = 0;
            let mut flags: u32 = 0;
            check(
                // SAFETY: the three out-pointers are live locals; no device or
                // QPC position is asked for.
                unsafe {
                    self.capture.GetBuffer(
                        &raw mut data,
                        &raw mut frames,
                        &raw mut flags,
                        None,
                        None,
                    )
                },
                "IAudioCaptureClient::GetBuffer",
            )?;
            // The flag constants are small positive bit masks.
            let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
            if flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0 && !self.first_packet {
                self.discontinuities += 1;
            }
            self.first_packet = false;
            let count = frames as usize;
            // Alignment is checked before the slice is made.
            #[allow(clippy::cast_ptr_alignment)]
            let floats = data.cast::<f32>();
            // A buffer not aligned for `f32` is delivered as silence of
            // its length, as `interleaved_view` does.
            let samples = if silent || floats.is_null() || !floats.is_aligned() || count == 0 {
                None
            } else {
                // SAFETY: `GetBuffer` succeeded, so `data` points at
                // `frames` frames in the format `initialize` gave the
                // client: 32-bit floats, `channels` interleaved; it is
                // aligned for `f32` (checked above). The engine neither
                // frees nor writes them until `ReleaseBuffer` below, and
                // the slice lives only for the `handle` call.
                Some(unsafe {
                    std::slice::from_raw_parts(floats.cast_const(), count * self.channels)
                })
            };
            handle(SliceView {
                frames: count,
                channels: self.channels,
                samples,
            });
            check(
                // SAFETY: releases exactly the packet `GetBuffer` lent; the
                // slice over it ended with the `handle` call.
                unsafe { self.capture.ReleaseBuffer(frames) },
                "IAudioCaptureClient::ReleaseBuffer",
            )?;
        }
    }
}

/// The process-loopback activation parameters and the `VT_BLOB` variant
/// pointing at them, together on the heap. Microsoft does not say whether
/// `ActivateAudioInterfaceAsync` copies them or reads them later on its
/// own thread (its ApplicationLoopback sample keeps them alive until the
/// completion), so the completion handler owns them: COM holds the handler
/// until the activation completes, even after a caller that timed out has
/// returned.
struct ActivationParams(NonNull<ActivationBlob>);

struct ActivationBlob {
    params: AUDIOCLIENT_ACTIVATION_PARAMS,
    /// Never dropped: `PROPVARIANT`'s `Drop` is `PropVariantClear`, which
    /// would `CoTaskMemFree` the blob, and the blob is `params` beside it:
    /// dropping it would free the blob twice.
    variant: ManuallyDrop<PROPVARIANT>,
}

impl ActivationParams {
    fn exclude_process_tree(pid: u32) -> Self {
        let blob = Box::new(ActivationBlob {
            params: AUDIOCLIENT_ACTIVATION_PARAMS {
                ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
                Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                    ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                        TargetProcessId: pid,
                        ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
                    },
                },
            },
            variant: ManuallyDrop::new(PROPVARIANT::default()),
        });
        let blob = NonNull::from(Box::leak(blob));
        let raw = blob.as_ptr();
        // SAFETY: `raw` is the live allocation just leaked, reached only
        // through this pointer. This writes the `VT_BLOB` arm of its zeroed
        // variant, pointing at its `params`, which live exactly as long.
        unsafe {
            let variant: &mut PROPVARIANT = &mut (*raw).variant;
            let inner = &mut *variant.Anonymous.Anonymous;
            inner.vt = VT_BLOB;
            inner.Anonymous.blob = BLOB {
                cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                pBlobData: (&raw mut (*raw).params).cast::<u8>(),
            };
        }
        Self(blob)
    }

    /// The variant to pass, valid while `self` lives.
    fn variant(&self) -> *const PROPVARIANT {
        // SAFETY: a place projection on the live allocation, no read;
        // `ManuallyDrop` is `repr(transparent)`.
        unsafe { (&raw const (*self.0.as_ptr()).variant).cast::<PROPVARIANT>() }
    }
}

impl Drop for ActivationParams {
    fn drop(&mut self) {
        // SAFETY: the allocation leaked in `exclude_process_tree`, freed
        // once; the variant inside is `ManuallyDrop`, so it is not cleared.
        drop(unsafe { Box::from_raw(self.0.as_ptr()) });
    }
}

/// `windows-implement` already answers `IAgileObject` for every
/// `#[implement]` type; it is listed here as well because
/// `ActivateAudioInterfaceAsync` requires an agile handler.
#[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
struct ActivationHandler {
    done: SyncSender<()>,
    /// Read by the activation until it completes; see [`ActivationParams`].
    _params: ActivationParams,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationHandler_Impl {
    fn ActivateCompleted(
        &self,
        _operation: Ref<IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        let _ = self.done.try_send(());
        Ok(())
    }
}

impl IAgileObject_Impl for ActivationHandler_Impl {}

/// `ActivateAudioInterfaceAsync` on the process-loopback virtual device
/// with `PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE`.
fn activate_process_loopback(
    excluded_pid: u32,
    timeout: Duration,
) -> Result<IAudioClient, ComError> {
    let params = ActivationParams::exclude_process_tree(excluded_pid);
    let variant = params.variant();
    let (sender, receiver) = sync_channel(1);
    let handler: IActivateAudioInterfaceCompletionHandler = ActivationHandler {
        done: sender,
        _params: params,
    }
    .into();
    // SAFETY: the device path is a static wide string, the IID is
    // `IAudioClient`'s and lives in a static, and the handler is a live,
    // agile COM object the call keeps a reference to until it completes.
    // The variant and its blob belong to the handler, so they stay valid
    // for as long as the activation runs, also if `recv_timeout` below
    // gives up first.
    let operation = check(
        unsafe {
            ActivateAudioInterfaceAsync(
                VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
                &IAudioClient::IID,
                Some(variant),
                &handler,
            )
        },
        "ActivateAudioInterfaceAsync",
    )?;
    if receiver.recv_timeout(timeout).is_err() {
        return Err(ComError::other(
            "ActivateAudioInterfaceAsync",
            format!("no completion within {timeout:?}"),
        ));
    }
    let mut result = HRESULT(0);
    let mut interface: Option<IUnknown> = None;
    check(
        // SAFETY: the operation completed (the handler ran); both out-pointers
        // are live locals.
        unsafe { operation.GetActivateResult(&raw mut result, &raw mut interface) },
        "IActivateAudioInterfaceAsyncOperation::GetActivateResult",
    )?;
    check(result.ok(), "process loopback activation")?;
    let interface = interface
        .ok_or_else(|| ComError::other("process loopback activation", "no interface returned"))?;
    check(
        interface.cast::<IAudioClient>(),
        "IUnknown::QueryInterface(IAudioClient)",
    )
}

/// MMCSS "Pro Audio" scheduling for the calling thread while the guard
/// lives; `None` when the service refuses (the thread runs at its normal
/// priority then).
pub struct ProAudioThread {
    handle: HANDLE,
    _thread: PhantomData<*const ()>,
}

impl ProAudioThread {
    /// Joins the "Pro Audio" task.
    #[must_use]
    pub fn enter() -> Option<Self> {
        let mut index: u32 = 0;
        // SAFETY: a static task name and a live index for the call to
        // write; the handle is reverted in `Drop` on this thread.
        let handle =
            unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &raw mut index) }.ok()?;
        Some(Self {
            handle,
            _thread: PhantomData,
        })
    }
}

impl Drop for ProAudioThread {
    fn drop(&mut self) {
        // SAFETY: reverts the handle this thread obtained, once.
        let _ = unsafe { AvRevertMmThreadCharacteristics(self.handle) };
    }
}

/// `IAudioSessionManager2` of one endpoint.
pub struct SessionManager(IAudioSessionManager2);

impl SessionManager {
    /// The endpoint's sessions right now. Calling it also makes the
    /// manager deliver `OnSessionCreated` afterwards, which it otherwise
    /// may not (`IAudioSessionNotification` remarks).
    pub fn sessions(&self) -> Result<Vec<Session>, ComError> {
        // SAFETY: plain calls on the live manager and its enumerator.
        let sessions = check(
            unsafe { self.0.GetSessionEnumerator() },
            "IAudioSessionManager2::GetSessionEnumerator",
        )?;
        // SAFETY: as above.
        let count = check(
            unsafe { sessions.GetCount() },
            "IAudioSessionEnumerator::GetCount",
        )?;
        Ok((0..count)
            // SAFETY: `index` is below the count the enumerator reported.
            .filter_map(|index| unsafe { sessions.GetSession(index) }.ok().map(Session))
            .collect())
    }

    /// Calls `on_created` whenever a session is created on the endpoint,
    /// until the registration is dropped.
    pub fn register_created(
        &self,
        on_created: Box<dyn Fn() + Send + Sync>,
    ) -> Result<SessionManagerRegistration, ComError> {
        let notification: IAudioSessionNotification = SessionCreated { on_created }.into();
        check(
            // SAFETY: a live COM object, unregistered in `Drop`.
            unsafe { self.0.RegisterSessionNotification(&notification) },
            "IAudioSessionManager2::RegisterSessionNotification",
        )?;
        Ok(SessionManagerRegistration {
            manager: self.0.clone(),
            notification,
        })
    }
}

#[implement(IAudioSessionNotification)]
struct SessionCreated {
    on_created: Box<dyn Fn() + Send + Sync>,
}

impl IAudioSessionNotification_Impl for SessionCreated_Impl {
    fn OnSessionCreated(&self, _session: Ref<IAudioSessionControl>) -> windows::core::Result<()> {
        (self.on_created)();
        Ok(())
    }
}

/// Unregisters the session-created notification on drop.
pub struct SessionManagerRegistration {
    manager: IAudioSessionManager2,
    notification: IAudioSessionNotification,
}

impl Drop for SessionManagerRegistration {
    fn drop(&mut self) {
        // SAFETY: undoes this registration on the same manager.
        let _ = unsafe {
            self.manager
                .UnregisterSessionNotification(&self.notification)
        };
    }
}

/// `IAudioSessionControl`: one session.
pub struct Session(IAudioSessionControl);

impl Session {
    /// The session as [`processes_from_sessions`](crate::detection::processes_from_sessions)
    /// reads it; `None` when its state cannot be read.
    #[must_use]
    pub fn record(&self, flow: EndpointFlow) -> Option<AudioSessionRecord> {
        // SAFETY: plain call on the live session.
        let state = unsafe { self.0.GetState() }.ok()?;
        let control: IAudioSessionControl2 = self.0.cast().ok()?;
        // A multi-process session answers `AUDCLNT_S_NO_SINGLE_PROCESS`,
        // a success code, with the creating process's id.
        // SAFETY: plain calls on the live session.
        let pid = unsafe { control.GetProcessId() }.unwrap_or(0);
        // SAFETY: as above; `S_OK` means it is the system-sounds session.
        let system_sounds = unsafe { control.IsSystemSoundsSession() } == S_OK;
        Some(AudioSessionRecord {
            pid,
            flow,
            state: session_state(state),
            system_sounds,
            image_path: if system_sounds || pid == 0 {
                None
            } else {
                process_image_path(pid)
            },
        })
    }

    /// Calls `on_state` whenever the session's state changes or it
    /// disconnects, until the registration is dropped.
    pub fn register_events(
        &self,
        on_state: Box<dyn Fn() + Send + Sync>,
    ) -> Result<SessionRegistration, ComError> {
        let events: IAudioSessionEvents = SessionEvents { on_state }.into();
        check(
            // SAFETY: a live COM object, unregistered in `Drop`.
            unsafe { self.0.RegisterAudioSessionNotification(&events) },
            "IAudioSessionControl::RegisterAudioSessionNotification",
        )?;
        Ok(SessionRegistration {
            session: self.0.clone(),
            events,
        })
    }
}

fn session_state(state: AudioSessionState) -> SessionState {
    if state == AudioSessionStateActive {
        SessionState::Active
    } else if state == AudioSessionStateExpired {
        SessionState::Expired
    } else {
        SessionState::Inactive
    }
}

#[implement(IAudioSessionEvents)]
struct SessionEvents {
    on_state: Box<dyn Fn() + Send + Sync>,
}

impl IAudioSessionEvents_Impl for SessionEvents_Impl {
    fn OnDisplayNameChanged(
        &self,
        _name: &PCWSTR,
        _context: *const GUID,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnIconPathChanged(
        &self,
        _path: &PCWSTR,
        _context: *const GUID,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnSimpleVolumeChanged(
        &self,
        _volume: f32,
        _mute: BOOL,
        _context: *const GUID,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnChannelVolumeChanged(
        &self,
        _count: u32,
        _volumes: *const f32,
        _changed: u32,
        _context: *const GUID,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnGroupingParamChanged(
        &self,
        _grouping: *const GUID,
        _context: *const GUID,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnStateChanged(&self, _state: AudioSessionState) -> windows::core::Result<()> {
        (self.on_state)();
        Ok(())
    }

    fn OnSessionDisconnected(
        &self,
        _reason: AudioSessionDisconnectReason,
    ) -> windows::core::Result<()> {
        (self.on_state)();
        Ok(())
    }
}

/// Unregisters the session events on drop.
pub struct SessionRegistration {
    session: IAudioSessionControl,
    events: IAudioSessionEvents,
}

impl Drop for SessionRegistration {
    fn drop(&mut self) {
        // SAFETY: undoes this registration on the same session.
        let _ = unsafe {
            self.session
                .UnregisterAudioSessionNotification(&self.events)
        };
    }
}

/// The full Win32 image path of `pid`, when the process may be queried
/// (`PROCESS_QUERY_LIMITED_INFORMATION`, which elevated processes also
/// grant).
#[must_use]
pub fn process_image_path(pid: u32) -> Option<String> {
    // SAFETY: plain values in; the handle is closed below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buffer = [0u16; 1_024];
    let mut size = buffer.len() as u32;
    // SAFETY: `buffer` is writable for `size` UTF-16 units, the call
    // writes at most that and stores the length written in `size`.
    let result = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &raw mut size,
        )
    };
    // SAFETY: the handle came from `OpenProcess` and is closed once.
    let _ = unsafe { CloseHandle(process) };
    result.ok()?;
    let written = buffer.get(..size as usize)?;
    Some(String::from_utf16_lossy(written))
}
