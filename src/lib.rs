//! WAVE is a macOS-oriented beat-inspection effect with a pass-through audio path.
//!
//! The audio callback captures only complete selected beat windows when the host
//! provides a usable tempo, musical position, and playing state. The editor
//! offers `1:4` (1 beat, default), `1:2` (2 beats), `1:1` (4 beats, 1 bar in
//! 4/4), and `2:1` (8 beats, 2 bars in 4/4) per instance; these labels are
//! explicit 4/4 descriptions, not time-signature decoding or parameters. The
//! retained Radiant editor
//! consumes bounded min/max envelopes published through atomic slots; it never
//! owns or mutates audio-side capture state.

#![deny(missing_docs, warnings)]

use std::marker::PhantomData;
use std::sync::Arc;

use toybox::clack_common::plugin::features as plugin_features;
use toybox::clack_extensions::audio_ports::*;
#[cfg(all(target_os = "macos", feature = "radiant-gui"))]
use toybox::clack_extensions::gui::{PluginGui, PluginGuiImpl};
use toybox::clack_extensions::params::{
    ParamDisplayWriter, ParamInfoWriter, PluginAudioProcessorParams, PluginMainThreadParams,
    PluginParams,
};
use toybox::clack_extensions::state::{PluginState, PluginStateImpl};
use toybox::clack_plugin::events::event_types::{TransportEvent, TransportFlags};
use toybox::clack_plugin::events::io::{InputEvents, OutputEvents};
use toybox::clack_plugin::prelude::*;
use toybox::clack_plugin::stream::{InputStream, OutputStream};
use toybox::clap::prelude::{ChannelPair, Process};
use toybox::clap::process::split_channel;
use toybox::clap::state::{read_versioned_payload, write_versioned_payload};

mod capture;
#[cfg(all(target_os = "macos", feature = "radiant-gui"))]
mod gui;
#[cfg(all(target_os = "macos", feature = "vst3"))]
mod vst3;

use capture::{CaptureEngine, TransportInfo, WaveformPublication};

/// CLAP plugin type for WAVE.
pub struct WavePlugin;

impl Plugin for WavePlugin {
    type AudioProcessor<'a> = WaveAudioProcessor<'a>;
    type Shared<'a> = WaveShared;
    type MainThread<'a> = WaveMainThread<'a>;

    fn declare_extensions(
        builder: &mut PluginExtensions<Self>,
        _shared: Option<&Self::Shared<'_>>,
    ) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginParams>()
            .register::<PluginState>();
        #[cfg(all(target_os = "macos", feature = "radiant-gui"))]
        builder.register::<PluginGui>();
    }
}

impl DefaultPluginFactory for WavePlugin {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new("com.portalsurfer.wave", "WAVE")
            .with_vendor("PORTALSURFER")
            .with_version("0.1.0")
            .with_description("Beat-synced waveform viewer with stereo pass-through")
            .with_features([plugin_features::AUDIO_EFFECT, plugin_features::STEREO])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<Self::Shared<'_>, PluginError> {
        Ok(WaveShared::new())
    }

    fn new_main_thread<'a>(
        host: HostMainThreadHandle<'a>,
        shared: &'a Self::Shared<'a>,
    ) -> Result<Self::MainThread<'a>, PluginError> {
        #[cfg(all(target_os = "macos", feature = "radiant-gui"))]
        {
            let _ = host;
            Ok(WaveMainThread {
                _marker: PhantomData,
                gui: gui::new_gui(Arc::clone(&shared.publication)),
            })
        }
        #[cfg(not(all(target_os = "macos", feature = "radiant-gui")))]
        {
            let _ = host;
            let _ = shared;
            Ok(WaveMainThread {
                _marker: PhantomData,
            })
        }
    }
}

/// Atomic state shared by the audio callback and retained editor.
pub struct WaveShared {
    publication: Arc<WaveformPublication>,
}

impl WaveShared {
    fn new() -> Self {
        Self {
            publication: Arc::new(WaveformPublication::new()),
        }
    }
}

impl PluginShared<'_> for WaveShared {}

/// Main-thread state for the CLAP host contract.
pub struct WaveMainThread<'a> {
    _marker: PhantomData<&'a WaveShared>,
    #[cfg(all(target_os = "macos", feature = "radiant-gui"))]
    gui: toybox::radiant_gui::RadiantHostedGui,
}

impl<'a> PluginMainThread<'a, WaveShared> for WaveMainThread<'a> {}

impl PluginAudioPortsImpl for WaveMainThread<'_> {
    fn count(&mut self, _is_input: bool) -> u32 {
        1
    }

    fn get(&mut self, index: u32, _is_input: bool, writer: &mut AudioPortInfoWriter) {
        if index != 0 {
            return;
        }
        writer.set(&AudioPortInfo {
            id: ClapId::new(0),
            name: b"main",
            channel_count: 2,
            flags: AudioPortFlags::IS_MAIN,
            port_type: Some(AudioPortType::STEREO),
            in_place_pair: None,
        });
    }
}

impl PluginMainThreadParams for WaveMainThread<'_> {
    fn count(&mut self) -> u32 {
        0
    }

    fn get_info(&mut self, _param_index: u32, _info: &mut ParamInfoWriter) {}

    fn get_value(&mut self, _param_id: ClapId) -> Option<f64> {
        None
    }

    fn value_to_text(
        &mut self,
        _param_id: ClapId,
        _value: f64,
        _writer: &mut ParamDisplayWriter,
    ) -> std::fmt::Result {
        Err(std::fmt::Error)
    }

    fn text_to_value(&mut self, _param_id: ClapId, _text: &std::ffi::CStr) -> Option<f64> {
        None
    }

    fn flush(
        &mut self,
        _input_parameter_changes: &InputEvents,
        _output_parameter_changes: &mut OutputEvents,
    ) {
    }
}

impl PluginStateImpl for WaveMainThread<'_> {
    fn save(&mut self, output: &mut OutputStream) -> Result<(), PluginError> {
        write_versioned_payload(output, STATE_MAGIC, STATE_VERSION, &[])?;
        Ok(())
    }

    fn load(&mut self, input: &mut InputStream) -> Result<(), PluginError> {
        let _ = read_versioned_payload(input, STATE_MAGIC, &[STATE_VERSION])?;
        Ok(())
    }
}

#[cfg(all(target_os = "macos", feature = "radiant-gui"))]
impl PluginGuiImpl for WaveMainThread<'_> {
    toybox::radiant_clap_gui_callbacks!(
        gui = gui,
        preferred_size = gui::preferred_window_size,
        show = |_main_thread| Ok(())
    );
}

/// Audio-thread processor for WAVE.
pub struct WaveAudioProcessor<'a> {
    shared: &'a WaveShared,
    capture: CaptureEngine,
}

impl<'a> PluginAudioProcessor<'a, WaveShared, WaveMainThread<'a>> for WaveAudioProcessor<'a> {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut WaveMainThread<'a>,
        shared: &'a WaveShared,
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        shared.publication.set_sample_rate(audio_config.sample_rate);
        Ok(Self {
            shared,
            capture: CaptureEngine::new(audio_config.sample_rate),
        })
    }

    fn process(
        &mut self,
        process: Process,
        mut audio: Audio,
        _events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        let transport = transport_from_clap(process.transport.copied());
        for mut port_pair in &mut audio {
            let Some(mut channels) = port_pair.channels()?.into_f32() else {
                continue;
            };
            let mut channel_iter = channels.iter_mut();
            let Some(left) = channel_iter.next() else {
                return Err(PluginError::Message("WAVE requires a stereo audio port"));
            };
            let Some(right) = channel_iter.next() else {
                return Err(PluginError::Message("WAVE requires a stereo audio port"));
            };
            if channel_iter.next().is_some() {
                return Err(PluginError::Message(
                    "WAVE accepts exactly two audio channels",
                ));
            }
            self.process_stereo_pair(left, right, transport);
            break;
        }
        Ok(ProcessStatus::Continue)
    }
}

impl WaveAudioProcessor<'_> {
    fn process_stereo_pair(
        &mut self,
        left: ChannelPair<'_, f32>,
        right: ChannelPair<'_, f32>,
        transport: TransportInfo,
    ) {
        let (left_input, mut left_output, _) = split_channel(left);
        let (right_input, mut right_output, _) = split_channel(right);

        if left_input.is_none()
            && let Some(output) = left_output.as_deref_mut()
        {
            output.fill(0.0);
        }
        if right_input.is_none()
            && let Some(output) = right_output.as_deref_mut()
        {
            output.fill(0.0);
        }

        let frames = channel_frames(left_input, left_output.as_deref())
            .min(channel_frames(right_input, right_output.as_deref()));
        if frames == 0 {
            self.capture
                .process_block(&[], &[], transport, &self.shared.publication);
            return;
        }

        if let (Some(input), Some(output)) = (left_input, left_output.as_deref_mut()) {
            output[..frames].copy_from_slice(&input[..frames]);
        }
        if let (Some(input), Some(output)) = (right_input, right_output.as_deref_mut()) {
            output[..frames].copy_from_slice(&input[..frames]);
        }

        let left_source = match (left_input, left_output.as_deref()) {
            (Some(input), _) => input,
            (None, Some(output)) => output,
            (None, None) => &[],
        };
        let right_source = match (right_input, right_output.as_deref()) {
            (Some(input), _) => input,
            (None, Some(output)) => output,
            (None, None) => &[],
        };

        self.capture.process_block(
            &left_source[..frames],
            &right_source[..frames],
            transport,
            &self.shared.publication,
        );
    }
}

fn channel_frames(input: Option<&[f32]>, output: Option<&[f32]>) -> usize {
    match (input, output) {
        (Some(input), Some(output)) => input.len().min(output.len()),
        (Some(input), None) => input.len(),
        (None, Some(output)) => output.len(),
        (None, None) => 0,
    }
}

impl PluginAudioProcessorParams for WaveAudioProcessor<'_> {
    fn flush(
        &mut self,
        _input_parameter_changes: &InputEvents,
        _output_parameter_changes: &mut OutputEvents,
    ) {
    }
}

fn transport_from_clap(transport: Option<TransportEvent>) -> TransportInfo {
    match transport {
        Some(event) => TransportInfo {
            tempo_bpm: event
                .flags
                .contains(TransportFlags::HAS_TEMPO)
                .then_some(event.tempo),
            song_pos_beats: event
                .flags
                .contains(TransportFlags::HAS_BEATS_TIMELINE)
                .then_some(event.song_pos_beats.to_float()),
            is_playing: event.flags.contains(TransportFlags::IS_PLAYING),
        },
        None => TransportInfo::default(),
    }
}

toybox::clap_plugin_entry!(WavePlugin);

const STATE_MAGIC: u32 = u32::from_le_bytes(*b"WAVE");
const STATE_VERSION: u32 = 1;
