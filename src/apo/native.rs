//! COM lifecycle and realtime buffer boundary for the capture-mode APO.
//! Windows serializes setup and processing; the callback uses preallocated state.
//! Pipe reconnects and all blocking work run on a separately counted DLL thread.
use super::{
    bridge::{Bridge, Exchange},
    pipe::Pipe,
    protocol::{self, LATENCY, MAX_CHANNELS, Packet, RATE},
};
use std::{
    cell::UnsafeCell,
    ffi::c_void,
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, AtomicUsize, Ordering},
    },
    time::Duration,
};
use windows::{
    Win32::{
        Foundation::{
            CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_INVALIDARG, E_OUTOFMEMORY,
            E_POINTER, HANDLE, S_FALSE, S_OK,
        },
        Media::Audio::{Apo::*, PKEY_AudioEndpoint_GUID},
        System::Com::{
            CoTaskMemAlloc, CoTaskMemFree, IClassFactory, IClassFactory_Impl,
            StructuredStorage::{PropVariantClear, PropVariantToStringAlloc},
        },
    },
    core::{BOOL, GUID, HRESULT, IUnknown, Interface, Ref, Result, implement},
};

const CLASS: GUID = GUID::from_u128(0x657b7d18_8316_4db1_9585_b981d14e615b);
const FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);
const MAX_CALLBACK_FRAMES: u32 = RATE as u32;
const RETRY_DELAY: Duration = Duration::from_millis(100);
static OBJECTS: AtomicUsize = AtomicUsize::new(0);
static THREADS: AtomicUsize = AtomicUsize::new(0);
static LOCKS: AtomicUsize = AtomicUsize::new(0);

struct Runtime {
    bridge: Bridge,
    exchange: Arc<Exchange>,
    channels: usize,
    max_frames: u32,
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.exchange.stop.store(true, Ordering::Release);
    }
}
struct ThreadCount;
impl Drop for ThreadCount {
    fn drop(&mut self) {
        THREADS.fetch_sub(1, Ordering::AcqRel);
    }
}
fn start_transport(name: String, exchange: Arc<Exchange>) -> std::io::Result<()> {
    // SAFETY: pin this module before a detached worker can execute its code.
    // COM unload queries alone cannot cover the final thread-return epilogue.
    // Windows releases the pinned module when the audio host process exits.
    unsafe {
        use windows::Win32::System::LibraryLoader::{
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_PIN,
            GetModuleHandleExW,
        };
        let mut module = windows::Win32::Foundation::HMODULE::default();
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
            windows::core::PCWSTR(start_transport as *const () as *const u16),
            &mut module,
        )
        .map_err(std::io::Error::other)?;
    }
    THREADS.fetch_add(1, Ordering::AcqRel);
    let result = std::thread::Builder::new()
        .name("clearwave-apo-ipc".into())
        .spawn(move || {
            let _count = ThreadCount;
            while !exchange.stop.load(Ordering::Acquire) {
                let Ok(mut pipe) = Pipe::client(&name) else {
                    std::thread::sleep(RETRY_DELAY);
                    continue;
                };
                let mut packet = Packet::default();
                while exchange.input.pop(&mut packet) {}
                exchange.connected.store(true, Ordering::Release);
                while !exchange.stop.load(Ordering::Acquire) {
                    exchange.input.discard_backlog(&mut packet);
                    if !exchange.input.pop(&mut packet) {
                        std::thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    let mut session = pipe.session(&exchange.stop);
                    if packet.write(&mut session).is_err() {
                        break;
                    }
                    let Ok(response) = Packet::read(&mut session) else {
                        break;
                    };
                    if response.sequence != protocol::NO_AUDIO {
                        exchange.output.push(&response);
                    }
                }
                exchange.connected.store(false, Ordering::Release);
            }
        });
    if result.is_err() {
        THREADS.fetch_sub(1, Ordering::AcqRel);
    }
    result.map(|_| ())
}

#[implement(
    IAudioProcessingObject,
    IAudioProcessingObjectConfiguration,
    IAudioProcessingObjectRT,
    IAudioSystemEffects,
    IAudioSystemEffects2
)]
struct CaptureApo {
    endpoint: Mutex<Option<String>>,
    runtime: UnsafeCell<Option<Runtime>>,
    channels: AtomicU32,
}
impl CaptureApo {
    fn new() -> Self {
        OBJECTS.fetch_add(1, Ordering::AcqRel);
        Self {
            endpoint: Mutex::new(None),
            runtime: UnsafeCell::new(None),
            channels: AtomicU32::new(1),
        }
    }
}
impl Drop for CaptureApo {
    fn drop(&mut self) {
        self.runtime.get_mut().take();
        OBJECTS.fetch_sub(1, Ordering::AcqRel);
    }
}
fn format(media: &Ref<IAudioMediaType>) -> Result<UNCOMPRESSEDAUDIOFORMAT> {
    let media = media
        .as_ref()
        .ok_or_else(|| windows::core::Error::from(E_POINTER))?;
    let mut format = UNCOMPRESSEDAUDIOFORMAT::default();
    // SAFETY: the borrowed COM interface fills a properly initialized structure.
    // The caller owns the interface for the duration of this synchronous query.
    // No media pointer or format buffer is retained by this helper.
    unsafe {
        media.GetUncompressedAudioFormat(&mut format)?;
    }
    if format.guidFormatType != FLOAT
        || format.dwBytesPerSampleContainer != 4
        || format.dwValidBitsPerSample != 32
        || format.fFramesPerSecond != RATE as f32
        || !(1..=MAX_CHANNELS as u32).contains(&format.dwSamplesPerFrame)
    {
        return Err(APOERR_FORMAT_NOT_SUPPORTED.into());
    }
    Ok(format)
}
fn support(
    opposite: Ref<IAudioMediaType>,
    requested: Ref<IAudioMediaType>,
) -> Result<IAudioMediaType> {
    let value = format(&requested)?;
    if opposite.as_ref().is_some() && value != format(&opposite)? {
        return Err(APOERR_FORMAT_NOT_SUPPORTED.into());
    }
    requested.cloned().ok_or_else(|| E_POINTER.into())
}
impl IAudioProcessingObject_Impl for CaptureApo_Impl {
    fn Reset(&self) -> Result<()> {
        // SAFETY: the APO host calls Reset outside the locked processing period.
        // Locked instances reject it instead of touching callback-owned state.
        // No worker is created or destroyed on the realtime thread.
        unsafe {
            if (*self.runtime.get()).is_some() {
                return Err(APOERR_APO_LOCKED.into());
            }
        }
        Ok(())
    }
    fn GetLatency(&self) -> Result<i64> {
        Ok((LATENCY as i64 * 10_000_000) / RATE as i64)
    }
    fn GetRegistrationProperties(&self) -> Result<*mut APO_REG_PROPERTIES> {
        let mut properties = APO_REG_PROPERTIES {
            clsid: CLASS,
            Flags: APO_FLAG(APO_FLAG_DEFAULT.0 | APO_FLAG_INPLACE.0),
            u32MajorVersion: 1,
            u32MinorVersion: 0,
            u32MinInputConnections: 1,
            u32MaxInputConnections: 1,
            u32MinOutputConnections: 1,
            u32MaxOutputConnections: 1,
            u32MaxInstances: u32::MAX,
            u32NumAPOInterfaces: 1,
            iidAPOInterfaceList: [IAudioProcessingObject::IID],
            ..Default::default()
        };
        for (target, value) in properties
            .szFriendlyName
            .iter_mut()
            .zip("Clearwave microphone processing".encode_utf16())
        {
            *target = value;
        }
        // SAFETY: the host receives a COM-allocated POD structure and owns freeing it.
        // A null allocation is reported before writing through the pointer.
        // Its fixed interface count matches the allocated registration structure.
        unsafe {
            let output =
                CoTaskMemAlloc(size_of::<APO_REG_PROPERTIES>()).cast::<APO_REG_PROPERTIES>();
            if output.is_null() {
                return Err(E_OUTOFMEMORY.into());
            }
            output.write(properties);
            Ok(output)
        }
    }
    fn Initialize(&self, size: u32, data: *const u8) -> Result<()> {
        if data.is_null() || size < size_of::<APOInitSystemEffects>() as u32 {
            return Err(E_INVALIDARG.into());
        }
        let mut endpoint = self
            .endpoint
            .lock()
            .map_err(|_| windows::core::Error::from(E_INVALIDARG))?;
        if endpoint.is_some() {
            return Err(APOERR_ALREADY_INITIALIZED.into());
        }
        // SAFETY: Windows supplies the sized system-effects initialization structure.
        // Its common prefix contains borrowed property-store interfaces; no ownership
        // is taken. Variant and returned UTF-16 allocations are explicitly released.
        unsafe {
            let initialization = &*data.cast::<APOInitSystemEffects>();
            if initialization.APOInit.cbSize != size || initialization.APOInit.clsid != CLASS {
                return Err(E_INVALIDARG.into());
            }
            let store = initialization
                .pAPOEndpointProperties
                .as_ref()
                .ok_or_else(|| windows::core::Error::from(E_POINTER))?;
            let mut value = store.GetValue(&PKEY_AudioEndpoint_GUID)?;
            let string = PropVariantToStringAlloc(&value);
            let _ = PropVariantClear(&mut value);
            let string = string?;
            let text = string.to_string();
            CoTaskMemFree(Some(string.0.cast()));
            *endpoint = Some(
                protocol::pipe_name(&text?)
                    .ok_or_else(|| windows::core::Error::from(E_INVALIDARG))?,
            );
        }
        Ok(())
    }
    fn IsInputFormatSupported(
        &self,
        opposite: Ref<IAudioMediaType>,
        requested: Ref<IAudioMediaType>,
    ) -> Result<IAudioMediaType> {
        support(opposite, requested)
    }
    fn IsOutputFormatSupported(
        &self,
        opposite: Ref<IAudioMediaType>,
        requested: Ref<IAudioMediaType>,
    ) -> Result<IAudioMediaType> {
        support(opposite, requested)
    }
    fn GetInputChannelCount(&self) -> Result<u32> {
        Ok(self.channels.load(Ordering::Relaxed))
    }
}
impl IAudioProcessingObjectConfiguration_Impl for CaptureApo_Impl {
    fn LockForProcess(
        &self,
        inputs: u32,
        input: *const *const APO_CONNECTION_DESCRIPTOR,
        outputs: u32,
        output: *const *const APO_CONNECTION_DESCRIPTOR,
    ) -> Result<()> {
        if inputs != 1 || outputs != 1 || input.is_null() || output.is_null() {
            return Err(APOERR_NUM_CONNECTIONS_INVALID.into());
        }
        let name = self
            .endpoint
            .lock()
            .map_err(|_| windows::core::Error::from(E_INVALIDARG))?
            .clone()
            .ok_or_else(|| windows::core::Error::from(APOERR_NOT_INITIALIZED))?;
        // SAFETY: the host serializes Lock/Unlock against realtime processing.
        // Descriptors and media types are borrowed only for format negotiation.
        // Mutable runtime ownership transfers to the callback until Unlock.
        unsafe {
            if (*self.runtime.get()).is_some() {
                return Err(APOERR_APO_LOCKED.into());
            }
            let input = (*input)
                .as_ref()
                .ok_or_else(|| windows::core::Error::from(E_POINTER))?;
            let output = (*output)
                .as_ref()
                .ok_or_else(|| windows::core::Error::from(E_POINTER))?;
            let source = input
                .pFormat
                .as_ref()
                .ok_or_else(|| windows::core::Error::from(E_POINTER))?;
            let destination = output
                .pFormat
                .as_ref()
                .ok_or_else(|| windows::core::Error::from(E_POINTER))?;
            let format = format(&source.into())?;
            support(source.into(), destination.into())?;
            if input.u32MaxFrameCount == 0
                || input.u32MaxFrameCount > MAX_CALLBACK_FRAMES
                || output.u32MaxFrameCount < input.u32MaxFrameCount
            {
                return Err(APOERR_INVALID_OUTPUT_MAXFRAMECOUNT.into());
            }
            let channels = format.dwSamplesPerFrame as usize;
            let exchange = Arc::new(Exchange::default());
            let runtime = Runtime {
                bridge: Bridge::new(channels)
                    .ok_or_else(|| windows::core::Error::from(APOERR_FORMAT_NOT_SUPPORTED))?,
                exchange: exchange.clone(),
                channels,
                max_frames: input.u32MaxFrameCount,
            };
            start_transport(name, exchange)
                .map_err(|_| windows::core::Error::from(E_OUTOFMEMORY))?;
            *self.runtime.get() = Some(runtime);
            self.channels.store(channels as u32, Ordering::Relaxed);
        }
        Ok(())
    }
    fn UnlockForProcess(&self) -> Result<()> {
        // SAFETY: Windows has stopped invoking APOProcess before Unlock.
        // Runtime destruction only signals the transport thread; it never joins it.
        // The DLL thread counter prevents unloading until transport exits.
        unsafe {
            if (*self.runtime.get()).take().is_none() {
                return Err(APOERR_ALREADY_UNLOCKED.into());
            }
        }
        Ok(())
    }
}
impl IAudioProcessingObjectRT_Impl for CaptureApo_Impl {
    fn APOProcess(
        &self,
        inputs: u32,
        input: *const *const APO_CONNECTION_PROPERTY,
        outputs: u32,
        output: *mut *mut APO_CONNECTION_PROPERTY,
    ) {
        if outputs != 1 || output.is_null() {
            return;
        }
        // SAFETY: the host provides valid audio buffers sized by LockForProcess.
        // Read each input frame before writing output, allowing in-place buffers.
        // The host grants one processing thread exclusive access to this runtime.
        unsafe {
            let input = if inputs == 1 && !input.is_null() {
                (*input).as_ref().copied()
            } else {
                None
            };
            let Some(output) = (*output).as_mut() else {
                return;
            };
            output.u32ValidFrameCount = 0;
            output.u32BufferFlags = BUFFER_INVALID;
            let Some(input) = input else {
                return;
            };
            let Some(runtime) = &mut *self.runtime.get() else {
                return;
            };
            let count = input.u32ValidFrameCount;
            if count > runtime.max_frames
                || output.pBuffer == 0
                || input.u32BufferFlags == BUFFER_INVALID
                || (input.pBuffer == 0 && input.u32BufferFlags != BUFFER_SILENT)
            {
                return;
            }
            for frame in 0..count as usize {
                let mut samples = [0.0; MAX_CHANNELS];
                if input.u32BufferFlags != BUFFER_SILENT {
                    for (channel, sample) in samples.iter_mut().enumerate().take(runtime.channels) {
                        *sample = (input.pBuffer as *const f32)
                            .add(frame * runtime.channels + channel)
                            .read();
                    }
                }
                let processed = runtime.bridge.frame(samples, &runtime.exchange);
                for (channel, sample) in processed.iter().enumerate().take(runtime.channels) {
                    (output.pBuffer as *mut f32)
                        .add(frame * runtime.channels + channel)
                        .write(*sample);
                }
            }
            output.u32ValidFrameCount = count;
            output.u32BufferFlags = BUFFER_VALID;
        }
    }
    fn CalcInputFrames(&self, frames: u32) -> u32 {
        frames
    }
    fn CalcOutputFrames(&self, frames: u32) -> u32 {
        frames
    }
}
impl IAudioSystemEffects_Impl for CaptureApo_Impl {}
impl IAudioSystemEffects2_Impl for CaptureApo_Impl {
    fn GetEffectsList(&self, effects: *mut *mut GUID, count: *mut u32, _: HANDLE) -> Result<()> {
        if effects.is_null() || count.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: these are caller-provided out parameters for the effects list.
        // An empty list avoids advertising processing while the worker is absent.
        // The audio path remains configured independently of effect discovery.
        unsafe {
            *effects = ptr::null_mut();
            *count = 0;
        }
        Ok(())
    }
}
#[implement(IClassFactory)]
struct Factory;
impl Drop for Factory {
    fn drop(&mut self) {
        OBJECTS.fetch_sub(1, Ordering::AcqRel);
    }
}
impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<IUnknown>,
        iid: *const GUID,
        output: *mut *mut c_void,
    ) -> Result<()> {
        if iid.is_null() || output.is_null() {
            return Err(E_POINTER.into());
        }
        if outer.as_ref().is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let object: IAudioProcessingObject = CaptureApo::new().into();
        // SAFETY: COM supplies valid interface-id and result pointers.
        // QueryInterface acquires the returned reference before the local drops.
        // Unsupported interfaces return the normal COM failure code.
        unsafe { object.query(iid, output).ok() }
    }
    fn LockServer(&self, lock: BOOL) -> Result<()> {
        if lock.as_bool() {
            LOCKS.fetch_add(1, Ordering::AcqRel);
        } else {
            let mut value = LOCKS.load(Ordering::Acquire);
            while value > 0 {
                match LOCKS.compare_exchange_weak(
                    value,
                    value - 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break,
                    Err(next) => value = next,
                }
            }
        }
        Ok(())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllGetClassObject(
    class: *const GUID,
    iid: *const GUID,
    output: *mut *mut c_void,
) -> HRESULT {
    if class.is_null() || iid.is_null() || output.is_null() {
        return E_POINTER;
    }
    // SAFETY: COM supplies valid class/interface identifiers and an output pointer.
    // Only Clearwave's own class is exposed, using windows-rs reference counting.
    // No system registration or audio-device mutation occurs while loading the DLL.
    unsafe {
        *output = ptr::null_mut();
        if *class != CLASS {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        OBJECTS.fetch_add(1, Ordering::AcqRel);
        let factory: IClassFactory = Factory.into();
        factory.query(iid, output)
    }
}
#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if OBJECTS.load(Ordering::Acquire) == 0
        && THREADS.load(Ordering::Acquire) == 0
        && LOCKS.load(Ordering::Acquire) == 0
    {
        S_OK
    } else {
        S_FALSE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::ManuallyDrop;
    use windows::Win32::{Foundation::E_NOTIMPL, Media::Audio::WAVEFORMATEX};

    #[implement(IAudioMediaType)]
    struct Media(UNCOMPRESSEDAUDIOFORMAT);
    impl IAudioMediaType_Impl for Media_Impl {
        fn IsCompressedFormat(&self) -> Result<BOOL> {
            Ok(false.into())
        }
        fn IsEqual(&self, _: Ref<IAudioMediaType>) -> Result<u32> {
            Err(E_NOTIMPL.into())
        }
        fn GetAudioFormat(&self) -> *mut WAVEFORMATEX {
            ptr::null_mut()
        }
        fn GetUncompressedAudioFormat(&self, output: *mut UNCOMPRESSEDAUDIOFORMAT) -> Result<()> {
            if output.is_null() {
                return Err(E_POINTER.into());
            }
            // SAFETY: the test calls this through a valid COM out parameter.
            // Media owns a complete plain-data format for the entire call.
            // No borrowed pointers are copied into the result.
            unsafe {
                output.write(self.0);
            }
            Ok(())
        }
    }
    fn media(rate: f32) -> IAudioMediaType {
        Media(UNCOMPRESSEDAUDIOFORMAT {
            guidFormatType: FLOAT,
            dwSamplesPerFrame: 2,
            dwBytesPerSampleContainer: 4,
            dwValidBitsPerSample: 32,
            fFramesPerSecond: rate,
            dwChannelMask: 3,
        })
        .into()
    }
    #[test]
    fn com_factory_negotiation_lock_and_in_place_dry_fallback() {
        // SAFETY: test-owned descriptors and sample arrays remain valid through
        // every synchronous COM call; no buffer is retained after Unlock.
        // The factory and registration allocations follow COM ownership rules.
        unsafe {
            let mut raw = ptr::null_mut();
            DllGetClassObject(&CLASS, &IClassFactory::IID, &mut raw)
                .ok()
                .unwrap();
            let factory = IClassFactory::from_raw(raw);
            let uninitialized: IAudioProcessingObject = factory.CreateInstance(None).unwrap();
            let properties = uninitialized.GetRegistrationProperties().unwrap();
            assert_eq!((*properties).clsid, CLASS);
            CoTaskMemFree(Some(properties.cast()));
            assert!(uninitialized.Initialize(&[]).is_err());

            let source = media(RATE as f32);
            assert!(
                uninitialized
                    .IsInputFormatSupported(&source, &media(44_100.0))
                    .is_err()
            );
            uninitialized
                .IsInputFormatSupported(&source, &source)
                .unwrap();
            use windows::Win32::{
                System::Com::StructuredStorage::InitPropVariantFromGUIDAsString,
                UI::Shell::PropertiesSystem::{IPropertyStore, PSCreateMemoryPropertyStore},
            };
            let mut store_raw = ptr::null_mut();
            PSCreateMemoryPropertyStore(&IPropertyStore::IID, &mut store_raw).unwrap();
            let store = IPropertyStore::from_raw(store_raw);
            let mut value = InitPropVariantFromGUIDAsString(&GUID::from_u128(
                0xaaaaaaaa_bbbb_cccc_dddd_eeeeeeeeeeee,
            ))
            .unwrap();
            store.SetValue(&PKEY_AudioEndpoint_GUID, &value).unwrap();
            PropVariantClear(&mut value).unwrap();
            let mut initialization = APOInitSystemEffects2 {
                APOInit: APOInitBaseStruct {
                    cbSize: size_of::<APOInitSystemEffects2>() as u32,
                    clsid: CLASS,
                },
                pAPOEndpointProperties: ManuallyDrop::new(Some(store)),
                ..Default::default()
            };
            let apo = uninitialized;
            let bytes = std::slice::from_raw_parts(
                (&initialization as *const APOInitSystemEffects2).cast::<u8>(),
                size_of::<APOInitSystemEffects2>(),
            );
            apo.Initialize(bytes).unwrap();
            assert!(apo.Initialize(bytes).is_err());
            ManuallyDrop::drop(&mut initialization.pAPOEndpointProperties);
            let configuration: IAudioProcessingObjectConfiguration = apo.cast().unwrap();
            let realtime: IAudioProcessingObjectRT = apo.cast().unwrap();
            let mut descriptor = APO_CONNECTION_DESCRIPTOR {
                pFormat: ManuallyDrop::new(Some(source)),
                u32MaxFrameCount: protocol::HOP as u32,
                ..Default::default()
            };
            let pointer = &descriptor as *const _;
            configuration
                .LockForProcess(&[pointer], &[pointer])
                .unwrap();
            assert!(
                configuration
                    .LockForProcess(&[pointer], &[pointer])
                    .is_err()
            );
            assert!(apo.Reset().is_err());
            assert_eq!(apo.GetLatency().unwrap(), 1_200_000);
            assert_eq!(realtime.CalcInputFrames(480), 480);
            assert_eq!(realtime.CalcOutputFrames(480), 480);

            let mut samples = [0.0f32; protocol::HOP * 2];
            for hop in 0..(LATENCY / protocol::HOP + 4) {
                for frame in samples.as_chunks_mut::<2>().0.iter_mut() {
                    frame.copy_from_slice(&[0.25, -0.5]);
                }
                let mut connection = APO_CONNECTION_PROPERTY {
                    pBuffer: samples.as_mut_ptr() as usize,
                    u32ValidFrameCount: protocol::HOP as u32,
                    u32BufferFlags: BUFFER_VALID,
                    ..Default::default()
                };
                let input = &connection as *const _;
                let mut output = &mut connection as *mut _;
                realtime.APOProcess(1, &input, 1, &mut output);
                assert_eq!(connection.u32ValidFrameCount, protocol::HOP as u32);
                for frame in samples.as_chunks::<2>().0 {
                    assert_eq!(
                        frame,
                        if hop < LATENCY / protocol::HOP {
                            &[0.0, 0.0]
                        } else {
                            &[0.25, -0.5]
                        }
                    );
                }
            }
            configuration.UnlockForProcess().unwrap();
            assert!(configuration.UnlockForProcess().is_err());
            apo.Reset().unwrap();
            ManuallyDrop::drop(&mut descriptor.pFormat);
        }
    }
}
