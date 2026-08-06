//! VST3 class factory for WAVE.

use std::ffi::c_void;

use toybox::vst3::prelude::Steinberg::*;
use toybox::vst3::prelude::*;

use super::{CONTROLLER_CID, PROCESSOR_CID, create_controller, create_processor, query_instance};

/// VST3 factory exposing the processor and edit controller classes.
#[derive(Default)]
pub(super) struct WaveVst3Factory;

impl Class for WaveVst3Factory {
    type Interfaces = (IPluginFactory,);
}

impl IPluginFactoryTrait for WaveVst3Factory {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        if info.is_null() {
            return kInvalidArgument;
        }
        let info = unsafe { &mut *info };
        copy_cstring("PORTALSURFER", &mut info.vendor);
        copy_cstring("https://github.com/PORTALSURFER/wave", &mut info.url);
        copy_cstring("support@portalsurfer.local", &mut info.email);
        info.flags = PFactoryInfo_::FactoryFlags_::kUnicode as int32;
        kResultOk
    }

    unsafe fn countClasses(&self) -> int32 {
        2
    }

    unsafe fn getClassInfo(&self, index: int32, info: *mut PClassInfo) -> tresult {
        if info.is_null() {
            return kInvalidArgument;
        }
        let info = unsafe { &mut *info };
        match index {
            0 => {
                write_class_info_many(info, PROCESSOR_CID, CATEGORY_AUDIO_MODULE_CLASS, "WAVE");
                kResultOk
            }
            1 => {
                write_class_info_many(
                    info,
                    CONTROLLER_CID,
                    CATEGORY_COMPONENT_CONTROLLER_CLASS,
                    "WAVE",
                );
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn createInstance(
        &self,
        cid: FIDString,
        iid: FIDString,
        object: *mut *mut c_void,
    ) -> tresult {
        if cid.is_null() || iid.is_null() || object.is_null() {
            return kInvalidArgument;
        }
        let class_id = unsafe { *(cid as *const TUID) };
        let Some(instance) = (match class_id {
            PROCESSOR_CID => create_processor(),
            CONTROLLER_CID => create_controller(),
            _ => None,
        }) else {
            return kInvalidArgument;
        };
        unsafe { query_instance(instance, iid, object) }
    }
}
