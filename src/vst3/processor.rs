//! VST3 processor and hardened stereo buffer boundary.

use std::cell::UnsafeCell;
use std::mem::{align_of, size_of};
use std::ops::{Deref, DerefMut};
use std::ptr;
use std::slice;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use toybox::vst3::prelude::Steinberg::*;
use toybox::vst3::prelude::*;

use super::CONTROLLER_CID;
use super::shared_state::{WaveVst3Runtime, WaveVst3Shared};
use super::transport::transport_from_process_context;

/// Exclusive realtime borrow of the Toybox audio runtime.
struct RealtimeRuntime {
    inner: UnsafeCell<AudioRuntime<WaveVst3Runtime>>,
    in_process: AtomicBool,
}

// SAFETY: the atomic flag permits at most one mutable runtime borrow. Lifecycle
// callbacks publish replacements and never access the audio-owned runtime.
unsafe impl Sync for RealtimeRuntime {}
unsafe impl Send for RealtimeRuntime {}

impl RealtimeRuntime {
    fn new(runtime: AudioRuntime<WaveVst3Runtime>) -> Self {
        Self {
            inner: UnsafeCell::new(runtime),
            in_process: AtomicBool::new(false),
        }
    }

    fn try_acquire(&self) -> Option<RealtimeRuntimeGuard<'_>> {
        self.in_process
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| RealtimeRuntimeGuard { owner: self })
    }
}

struct RealtimeRuntimeGuard<'a> {
    owner: &'a RealtimeRuntime,
}

impl Deref for RealtimeRuntimeGuard<'_> {
    type Target = AudioRuntime<WaveVst3Runtime>;

    fn deref(&self) -> &Self::Target {
        // SAFETY: successful guard acquisition gives this guard exclusive access.
        unsafe { &*self.owner.inner.get() }
    }
}

impl DerefMut for RealtimeRuntimeGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: see the `Deref` implementation above.
        unsafe { &mut *self.owner.inner.get() }
    }
}

impl Drop for RealtimeRuntimeGuard<'_> {
    fn drop(&mut self) {
        self.owner.in_process.store(false, Ordering::Release);
    }
}

/// Validated stereo f32 pointers for one VST3 process block.
#[derive(Clone, Copy)]
struct RawStereoBuffers {
    frames: usize,
    input_left: *const f32,
    input_right: *const f32,
    output_left: *mut f32,
    output_right: *mut f32,
    input_silence_flags: uint64,
}

fn address_range<T>(pointer: *const T, count: usize) -> Option<(usize, usize)> {
    if pointer.is_null() || !(pointer as usize).is_multiple_of(align_of::<T>()) {
        return None;
    }
    let bytes = count.checked_mul(size_of::<T>())?;
    let start = pointer as usize;
    Some((start, start.checked_add(bytes)?))
}

fn ranges_overlap(first: (usize, usize), second: (usize, usize)) -> bool {
    first.0 < second.1 && second.0 < first.1
}

fn validate_stereo_aliases(
    input_left: *const f32,
    input_right: *const f32,
    output_left: *mut f32,
    output_right: *mut f32,
    frames: usize,
) -> bool {
    let Some(input_left_range) = address_range(input_left, frames) else {
        return false;
    };
    let Some(input_right_range) = address_range(input_right, frames) else {
        return false;
    };
    let Some(output_left_range) = address_range(output_left, frames) else {
        return false;
    };
    let Some(output_right_range) = address_range(output_right, frames) else {
        return false;
    };

    if ranges_overlap(input_left_range, input_right_range)
        || ranges_overlap(output_left_range, output_right_range)
        || ranges_overlap(output_left_range, input_right_range)
        || ranges_overlap(output_right_range, input_left_range)
    {
        return false;
    }

    let left_overlap = ranges_overlap(output_left_range, input_left_range);
    let right_overlap = ranges_overlap(output_right_range, input_right_range);
    (!left_overlap || ptr::eq(output_left as *const f32, input_left))
        && (!right_overlap || ptr::eq(output_right as *const f32, input_right))
}

/// Read exactly one stereo f32 input and output bus after validating pointers.
unsafe fn raw_stereo_buffers(data: &ProcessData) -> Option<RawStereoBuffers> {
    if data.numInputs != 1
        || data.numOutputs != 1
        || data.inputs.is_null()
        || data.outputs.is_null()
        || address_range(data.inputs.cast_const(), 1).is_none()
        || address_range(data.outputs.cast_const(), 1).is_none()
    {
        return None;
    }
    let input = unsafe { &*data.inputs };
    let output = unsafe { &*data.outputs };
    let input_channels_ptr = unsafe { input.__field0.channelBuffers32 };
    let output_channels_ptr = unsafe { output.__field0.channelBuffers32 };
    if input.numChannels != 2
        || output.numChannels != 2
        || input_channels_ptr.is_null()
        || output_channels_ptr.is_null()
        || address_range(input_channels_ptr.cast_const(), 2).is_none()
        || address_range(output_channels_ptr.cast_const(), 2).is_none()
    {
        return None;
    }

    let input_channels = unsafe { slice::from_raw_parts(input_channels_ptr, 2) };
    let output_channels = unsafe { slice::from_raw_parts(output_channels_ptr, 2) };
    if input_channels.iter().any(|channel| channel.is_null())
        || output_channels.iter().any(|channel| channel.is_null())
    {
        return None;
    }
    let frames = usize::try_from(data.numSamples).ok()?;
    if !validate_stereo_aliases(
        input_channels[0],
        input_channels[1],
        output_channels[0],
        output_channels[1],
        frames,
    ) {
        return None;
    }
    Some(RawStereoBuffers {
        frames,
        input_left: input_channels[0],
        input_right: input_channels[1],
        output_left: output_channels[0],
        output_right: output_channels[1],
        input_silence_flags: input.silenceFlags,
    })
}

/// Silence a structurally valid stereo output when the input descriptor is bad.
unsafe fn silence_valid_stereo_output(data: &ProcessData) -> tresult {
    if data.symbolicSampleSize != SymbolicSampleSizes_::kSample32 as int32
        || data.numOutputs != 1
        || data.outputs.is_null()
    {
        return kInvalidArgument;
    }
    let frames = match usize::try_from(data.numSamples) {
        Ok(frames) => frames,
        Err(_) => return kInvalidArgument,
    };
    if address_range(data.outputs.cast_const(), 1).is_none() {
        return kInvalidArgument;
    }
    let output = unsafe { &mut *data.outputs };
    let channels_ptr = unsafe { output.__field0.channelBuffers32 };
    if output.numChannels != 2
        || channels_ptr.is_null()
        || address_range(channels_ptr.cast_const(), 2).is_none()
    {
        return kInvalidArgument;
    }
    let channels = unsafe { slice::from_raw_parts(channels_ptr, 2) };
    if channels.iter().any(|channel| channel.is_null()) {
        return kInvalidArgument;
    }
    let Some(left_range) = address_range(channels[0], frames) else {
        return kInvalidArgument;
    };
    let Some(right_range) = address_range(channels[1], frames) else {
        return kInvalidArgument;
    };
    if ranges_overlap(left_range, right_range) {
        return kInvalidArgument;
    }
    for channel in channels {
        unsafe { slice::from_raw_parts_mut(*channel, frames).fill(0.0) };
    }
    output.silenceFlags = 0b11;
    kInvalidArgument
}

impl RawStereoBuffers {
    /// Copy input to output, preserving exact in-place aliases.
    unsafe fn passthrough(self) {
        if !ptr::eq(self.input_left, self.output_left as *const f32) {
            unsafe { ptr::copy_nonoverlapping(self.input_left, self.output_left, self.frames) };
        }
        if !ptr::eq(self.input_right, self.output_right as *const f32) {
            unsafe { ptr::copy_nonoverlapping(self.input_right, self.output_right, self.frames) };
        }
    }
}

/// VST3 processor component for WAVE.
pub(super) struct WaveVst3Processor {
    connection: InstanceConnection<WaveVst3Shared>,
    /// Processor-owned canonical state; process never resolves it through the connection.
    shared: Arc<WaveVst3Shared>,
    runtime: RealtimeRuntime,
    publisher: RuntimePublisher<WaveVst3Runtime>,
    processing_reset_requested: AtomicBool,
}

impl WaveVst3Processor {
    /// Create a processor with a fully initialized default runtime.
    pub(super) fn new() -> Self {
        let shared = WaveVst3Shared::new();
        let (publisher, runtime) = RuntimePublisher::new(WaveVst3Runtime::new(48_000.0));
        Self {
            connection: InstanceConnection::new(
                InstanceConnectionRole::Processor,
                Arc::clone(&shared),
            ),
            shared,
            runtime: RealtimeRuntime::new(runtime),
            publisher,
            processing_reset_requested: AtomicBool::new(false),
        }
    }

    fn publish_runtime(&self, sample_rate: f64) {
        self.shared.publication.set_sample_rate(sample_rate);
        let Ok(registration) = self.publisher.register() else {
            return;
        };
        registration.publish(WaveVst3Runtime::new(sample_rate));
        let _ = self.publisher.reclaim();
    }
}

impl Drop for WaveVst3Processor {
    fn drop(&mut self) {
        let _ = self.publisher.reclaim();
    }
}

impl Class for WaveVst3Processor {
    type Interfaces = (
        IComponent,
        IAudioProcessor,
        IProcessContextRequirements,
        IConnectionPoint,
        IToyboxSharedState,
    );
}

toybox::impl_vst3_instance_connection!(WaveVst3Processor, connection);

impl IPluginBaseTrait for WaveVst3Processor {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IComponentTrait for WaveVst3Processor {
    unsafe fn getControllerClassId(&self, class_id: *mut TUID) -> tresult {
        if class_id.is_null() {
            return kInvalidArgument;
        }
        unsafe { *class_id = CONTROLLER_CID };
        kResultOk
    }

    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }

    unsafe fn getBusCount(&self, media_type: MediaType, direction: BusDirection) -> int32 {
        match media_type as MediaTypes {
            MediaTypes_::kAudio => match direction as BusDirections {
                BusDirections_::kInput | BusDirections_::kOutput => 1,
                _ => 0,
            },
            _ => 0,
        }
    }

    #[cfg_attr(not(target_os = "windows"), allow(clippy::unnecessary_cast))]
    unsafe fn getBusInfo(
        &self,
        media_type: MediaType,
        direction: BusDirection,
        index: int32,
        bus: *mut BusInfo,
    ) -> tresult {
        if bus.is_null() || index != 0 || media_type as MediaTypes != MediaTypes_::kAudio {
            return kInvalidArgument;
        }
        if !matches!(
            direction as BusDirections,
            BusDirections_::kInput | BusDirections_::kOutput
        ) {
            return kInvalidArgument;
        }
        let bus = unsafe { &mut *bus };
        bus.mediaType = MediaTypes_::kAudio as MediaType;
        bus.direction = direction;
        bus.channelCount = 2;
        copy_wstring(
            if direction as BusDirections == BusDirections_::kInput {
                "Input"
            } else {
                "Output"
            },
            &mut bus.name,
        );
        bus.busType = BusTypes_::kMain as BusType;
        bus.flags = BusInfo_::BusFlags_::kDefaultActive as u32;
        kResultOk
    }

    unsafe fn getRoutingInfo(
        &self,
        _input: *mut RoutingInfo,
        _output: *mut RoutingInfo,
    ) -> tresult {
        kNotImplemented
    }

    unsafe fn activateBus(
        &self,
        media_type: MediaType,
        direction: BusDirection,
        index: int32,
        _state: TBool,
    ) -> tresult {
        if media_type as MediaTypes != MediaTypes_::kAudio
            || index != 0
            || !matches!(
                direction as BusDirections,
                BusDirections_::kInput | BusDirections_::kOutput
            )
        {
            return kInvalidArgument;
        }
        kResultOk
    }

    unsafe fn setActive(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }
}

impl IAudioProcessorTrait for WaveVst3Processor {
    unsafe fn setBusArrangements(
        &self,
        inputs: *mut SpeakerArrangement,
        input_count: int32,
        outputs: *mut SpeakerArrangement,
        output_count: int32,
    ) -> tresult {
        if input_count != 1 || output_count != 1 || inputs.is_null() || outputs.is_null() {
            return kResultFalse;
        }
        if unsafe { *inputs } != SpeakerArr::kStereo || unsafe { *outputs } != SpeakerArr::kStereo {
            return kResultFalse;
        }
        kResultTrue
    }

    unsafe fn getBusArrangement(
        &self,
        direction: BusDirection,
        index: int32,
        arrangement: *mut SpeakerArrangement,
    ) -> tresult {
        if arrangement.is_null() || index != 0 {
            return kInvalidArgument;
        }
        match direction as BusDirections {
            BusDirections_::kInput | BusDirections_::kOutput => {
                unsafe { *arrangement = SpeakerArr::kStereo };
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn canProcessSampleSize(&self, sample_size: int32) -> tresult {
        match sample_size as SymbolicSampleSizes {
            SymbolicSampleSizes_::kSample32 => kResultOk,
            SymbolicSampleSizes_::kSample64 => kNotImplemented,
            _ => kInvalidArgument,
        }
    }

    unsafe fn getLatencySamples(&self) -> uint32 {
        0
    }

    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        let Some(setup) = (unsafe { setup.as_ref() }) else {
            return kInvalidArgument;
        };
        if !setup.sampleRate.is_finite() || setup.sampleRate <= 0.0 {
            return kInvalidArgument;
        }
        self.publish_runtime(setup.sampleRate);
        kResultOk
    }

    unsafe fn setProcessing(&self, state: TBool) -> tresult {
        if state == 0 {
            self.shared.publication.clear_live_preview();
        }
        self.processing_reset_requested
            .store(true, Ordering::Release);
        kResultOk
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        if address_range(data.cast_const(), 1).is_none() {
            return kInvalidArgument;
        }
        let Some(data) = (unsafe { data.as_ref() }) else {
            return kInvalidArgument;
        };
        if data.numSamples < 0 {
            return kInvalidArgument;
        }
        let frames = data.numSamples as usize;
        let Some(mut guard) = self.runtime.try_acquire() else {
            return if frames == 0 {
                kResultOk
            } else {
                unsafe { silence_valid_stereo_output(data) }
            };
        };
        let _ = guard.try_adopt(|_, _| true);
        let runtime = guard.current_mut();
        if self
            .processing_reset_requested
            .swap(false, Ordering::AcqRel)
        {
            runtime.capture.reset(&self.shared.publication);
        }
        let transport = unsafe { transport_from_process_context(data.processContext) };
        if frames == 0 {
            runtime
                .capture
                .process_block(&[], &[], transport, &self.shared.publication);
            return kResultOk;
        }
        if data.symbolicSampleSize != SymbolicSampleSizes_::kSample32 as int32 {
            return unsafe { silence_valid_stereo_output(data) };
        }
        let Some(buffers) = (unsafe { raw_stereo_buffers(data) }) else {
            return unsafe { silence_valid_stereo_output(data) };
        };
        unsafe { buffers.passthrough() };
        // SAFETY: raw_stereo_buffers validated the output bus pointer and its
        // channel storage before the pass-through copy.
        unsafe {
            (*data.outputs).silenceFlags = buffers.input_silence_flags;
        }
        let (left, right) = unsafe {
            (
                slice::from_raw_parts(buffers.input_left, buffers.frames),
                slice::from_raw_parts(buffers.input_right, buffers.frames),
            )
        };
        runtime
            .capture
            .process_block(left, right, transport, &self.shared.publication);
        kResultOk
    }

    unsafe fn getTailSamples(&self) -> uint32 {
        0
    }
}

impl IProcessContextRequirementsTrait for WaveVst3Processor {
    #[cfg_attr(not(target_os = "windows"), allow(clippy::unnecessary_cast))]
    unsafe fn getProcessContextRequirements(&self) -> uint32 {
        (IProcessContextRequirements_::Flags_::kNeedTempo as u32)
            | (IProcessContextRequirements_::Flags_::kNeedProjectTimeMusic as u32)
            | (IProcessContextRequirements_::Flags_::kNeedTransportState as u32)
    }
}

#[cfg(test)]
mod tests {
    use std::mem;
    use std::sync::atomic::Ordering;

    use super::{WaveVst3Processor, raw_stereo_buffers, validate_stereo_aliases};
    use crate::capture::{
        ENVELOPE_BINS, EnvelopePoint, SnapshotMode, TransportInfo, WaveformView, WindowLength,
    };
    use toybox::vst3::prelude::Steinberg::*;
    use toybox::vst3::prelude::*;

    struct StereoProcessFixture {
        process_data: ProcessData,
        _input_left: Vec<f32>,
        _input_right: Vec<f32>,
        output_left: Vec<f32>,
        output_right: Vec<f32>,
        _input_channel_buffers: Vec<*mut f32>,
        _output_channel_buffers: Vec<*mut f32>,
        _input_buses: Vec<AudioBusBuffers>,
        output_buses: Vec<AudioBusBuffers>,
    }

    fn stereo_process_fixture(samples: usize) -> StereoProcessFixture {
        let mut input_left = vec![0.0; samples];
        let mut input_right: Vec<f32> = (0..samples).map(|sample| sample as f32 + 1.0).collect();
        let mut output_left = vec![9.0; samples];
        let mut output_right = vec![9.0; samples];
        let mut input_channel_buffers = vec![input_left.as_mut_ptr(), input_right.as_mut_ptr()];
        let mut output_channel_buffers = vec![output_left.as_mut_ptr(), output_right.as_mut_ptr()];
        let input_bus = AudioBusBuffers {
            numChannels: 2,
            silenceFlags: 0b01,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: input_channel_buffers.as_mut_ptr(),
            },
        };
        let output_bus = AudioBusBuffers {
            numChannels: 2,
            silenceFlags: 0b11,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: output_channel_buffers.as_mut_ptr(),
            },
        };
        let mut input_buses = vec![input_bus];
        let mut output_buses = vec![output_bus];
        // SAFETY: the test overwrites the fields needed for a valid VST3
        // stereo process block below.
        let mut process_data: ProcessData = unsafe { mem::zeroed() };
        process_data.symbolicSampleSize = SymbolicSampleSizes_::kSample32 as int32;
        process_data.numSamples = samples as int32;
        process_data.numInputs = 1;
        process_data.numOutputs = 1;
        process_data.inputs = input_buses.as_mut_ptr();
        process_data.outputs = output_buses.as_mut_ptr();
        StereoProcessFixture {
            process_data,
            _input_left: input_left,
            _input_right: input_right,
            output_left,
            output_right,
            _input_channel_buffers: input_channel_buffers,
            _output_channel_buffers: output_channel_buffers,
            _input_buses: input_buses,
            output_buses,
        }
    }

    #[test]
    fn exact_in_place_stereo_aliases_are_allowed() {
        let left = 0x1000usize as *const f32;
        let right = 0x2000usize as *const f32;
        assert!(validate_stereo_aliases(
            left,
            right,
            left as *mut f32,
            right as *mut f32,
            64,
        ));
    }

    #[test]
    fn cross_channel_aliases_are_rejected() {
        let left = 0x1000usize as *const f32;
        let right = 0x2000usize as *const f32;
        assert!(!validate_stereo_aliases(
            left,
            right,
            right as *mut f32,
            left as *mut f32,
            64,
        ));
    }

    #[test]
    fn process_propagates_input_silence_flags_after_pass_through() {
        let processor = WaveVst3Processor::new();
        let mut fixture = stereo_process_fixture(8);

        assert_eq!(fixture.output_buses[0].silenceFlags, 0b11);
        let result = unsafe { processor.process(&mut fixture.process_data) };

        assert_eq!(result, kResultOk);
        assert_eq!(fixture.output_buses[0].silenceFlags, 0b01);
        assert_eq!(fixture.output_left, fixture._input_left);
        assert_eq!(fixture.output_right, fixture._input_right);
    }

    #[test]
    fn raw_stereo_buffers_rejects_misaligned_channel_pointer_array() {
        let mut fixture = stereo_process_fixture(8);
        let misaligned = fixture
            ._input_channel_buffers
            .as_mut_ptr()
            .cast::<u8>()
            .wrapping_add(1)
            .cast::<*mut f32>();
        fixture._input_buses[0].__field0 = AudioBusBuffers__type0 {
            channelBuffers32: misaligned,
        };

        assert!(unsafe { raw_stereo_buffers(&fixture.process_data) }.is_none());
    }

    #[test]
    fn process_rejects_misaligned_sample_pointer_without_touching_output() {
        let processor = WaveVst3Processor::new();
        let mut fixture = stereo_process_fixture(8);
        fixture._output_channel_buffers[0] = fixture
            .output_left
            .as_mut_ptr()
            .cast::<u8>()
            .wrapping_add(1)
            .cast::<f32>();

        let result = unsafe { processor.process(&mut fixture.process_data) };

        assert_eq!(result, kInvalidArgument);
        assert_eq!(fixture.output_buses[0].silenceFlags, 0b11);
        assert_eq!(fixture.output_left, vec![9.0; 8]);
        assert_eq!(fixture.output_right, vec![9.0; 8]);
    }

    #[test]
    fn set_processing_requests_reset_without_replacing_the_runtime() {
        let processor = WaveVst3Processor::new();
        let runtime = processor.runtime.inner.get();
        let envelope = [EnvelopePoint {
            min: -0.5,
            max: 0.5,
        }; ENVELOPE_BINS];
        processor.shared.publication.publish_envelope(
            SnapshotMode::Synced,
            WindowLength::DEFAULT,
            Some(60.0),
            Some(0),
            32,
            &envelope,
        );
        processor.shared.publication.publish_live_preview(
            TransportInfo {
                tempo_bpm: Some(60.0),
                song_pos_beats: Some(0.25),
                is_playing: true,
            },
            SnapshotMode::Synced,
            WindowLength::DEFAULT,
            Some(60.0),
            Some(0),
            8,
            32,
            &envelope,
        );
        let mut seeded = WaveformView::default();
        assert!(processor.shared.publication.read_view(&mut seeded));
        assert!(seeded.live_valid);
        let completed_revision = seeded.snapshot_revision;

        unsafe { processor.setProcessing(0) };

        assert_eq!(processor.runtime.inner.get(), runtime);
        assert!(processor.processing_reset_requested.load(Ordering::Acquire));
        let mut cleared = WaveformView::default();
        assert!(processor.shared.publication.read_view(&mut cleared));
        assert!(!cleared.live_valid);
        assert_eq!(cleared.snapshot_revision, completed_revision);
        assert!(cleared.display_valid);
        assert_eq!(cleared.display_sample_count, 8);
        assert_eq!(cleared.display_envelope, envelope);
    }
}
