//! Cross-platform VST3 adapter for WAVE.

use std::ffi::c_void;

use toybox::vst3::prelude::Steinberg::*;
use toybox::vst3::prelude::*;

mod controller;
mod factory;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod gui_adapter;
mod processor;
mod shared_state;
mod transport;

use controller::WaveVst3Controller;
use factory::WaveVst3Factory;
use processor::WaveVst3Processor;

/// Stable VST3 processor class identifier.
pub(super) const PROCESSOR_CID: TUID = uid(0xD5A0B6E1, 0x4B1248C7, 0xA1D0D2AA, 0x6B7B1D13);
/// Stable VST3 edit-controller class identifier.
pub(super) const CONTROLLER_CID: TUID = uid(0xA4E85C2D, 0xB26A4F54, 0x9D4F6C11, 0xE8A7D0B2);

/// Create a VST3 processor instance for the class factory.
pub(super) fn create_processor() -> Option<ComPtr<FUnknown>> {
    ComWrapper::new(WaveVst3Processor::new()).to_com_ptr::<FUnknown>()
}

/// Create a VST3 controller instance for the class factory.
pub(super) fn create_controller() -> Option<ComPtr<FUnknown>> {
    ComWrapper::new(WaveVst3Controller::new()).to_com_ptr::<FUnknown>()
}

/// Query a class instance for the requested VST3 interface.
pub(super) unsafe fn query_instance(
    instance: ComPtr<FUnknown>,
    iid: FIDString,
    object: *mut *mut c_void,
) -> tresult {
    let pointer = instance.as_ptr();
    unsafe { ((*(*pointer).vtbl).queryInterface)(pointer, iid as *mut TUID, object) }
}

toybox::vst3_plugin_entry!(WaveVst3Factory);
