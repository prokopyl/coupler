use crate::{EditorContext, EditorContextInner, ParamChange, ParamId, ParentWindow, Plugin};

use std::cell::{Cell, UnsafeCell};
use std::ffi::c_void;
use std::marker::PhantomData;
use std::os::raw::{c_char, c_int};
use std::rc::Rc;
use std::sync::atomic;
use std::sync::atomic::{AtomicU32, Ordering};
use std::{ffi, mem, ptr, slice};

use raw_window_handle::RawWindowHandle;

pub use vst3 as vst3_api;
use vst3::*;

fn copy_cstring(src: &str, dst: &mut [c_char]) {
    let c_string = ffi::CString::new(src).unwrap_or_else(|_| ffi::CString::default());
    for (src, dst) in c_string.as_bytes_with_nul().iter().zip(dst.iter_mut()) {
        *dst = *src as c_char;
    }
}

fn copy_wstring(src: &str, dst: &mut [i16]) {
    let mut len = 0;
    for (src, dst) in src.encode_utf16().zip(dst.iter_mut()) {
        *dst = src as i16;
        len += 1;
    }
    if len < dst.len() {
        dst[len] = 0;
    }
}

unsafe fn len_wstring(string: *const i16) -> usize {
    let mut len = 0;
    while *string.offset(len) != 0 {
        len += 1;
    }
    len as usize
}

#[repr(C)]
pub struct Factory<P> {
    pub plugin_factory_3: *const IPluginFactory3,
    pub component: *const IComponent,
    pub audio_processor: *const IAudioProcessor,
    pub process_context_requirements: *const IProcessContextRequirements,
    pub edit_controller: *const IEditController,
    pub plug_view: *const IPlugView,
    pub event_handler: *const IEventHandler,
    pub timer_handler: *const ITimerHandler,
    pub phantom: PhantomData<P>,
}

unsafe impl<P> Sync for Factory<P> {}

impl<P: Plugin> Factory<P> {
    pub unsafe extern "system" fn query_interface(
        this: *mut c_void,
        iid: *const TUID,
        obj: *mut *mut c_void,
    ) -> TResult {
        let iid = *iid;

        if iid == FUnknown::IID
            || iid == IPluginFactory::IID
            || iid == IPluginFactory2::IID
            || iid == IPluginFactory3::IID
        {
            *obj = this;
            return result::OK;
        }

        result::NO_INTERFACE
    }

    pub unsafe extern "system" fn add_ref(_this: *mut c_void) -> u32 {
        1
    }

    pub unsafe extern "system" fn release(_this: *mut c_void) -> u32 {
        1
    }

    pub unsafe extern "system" fn get_factory_info(
        _this: *mut c_void,
        info: *mut PFactoryInfo,
    ) -> TResult {
        let info = &mut *info;

        copy_cstring(P::INFO.vendor, &mut info.vendor);
        copy_cstring(P::INFO.url, &mut info.url);
        copy_cstring(P::INFO.email, &mut info.email);
        info.flags = PFactoryInfo::UNICODE;

        result::OK
    }

    pub unsafe extern "system" fn count_classes(_this: *mut c_void) -> i32 {
        1
    }

    pub unsafe extern "system" fn get_class_info(
        _this: *mut c_void,
        index: i32,
        info: *mut PClassInfo,
    ) -> TResult {
        if index != 0 {
            return result::INVALID_ARGUMENT;
        }

        let info = &mut *info;

        info.cid = iid(P::INFO.uid[0], P::INFO.uid[1], P::INFO.uid[2], P::INFO.uid[3]);
        info.cardinality = PClassInfo::MANY_INSTANCES;
        copy_cstring("Audio Module Class", &mut info.category);
        copy_cstring(P::INFO.name, &mut info.name);

        result::OK
    }

    pub unsafe extern "system" fn create_instance(
        this: *mut c_void,
        cid: *const c_char,
        iid: *const c_char,
        obj: *mut *mut c_void,
    ) -> TResult {
        let cid = *(cid as *const TUID);
        let iid = *(iid as *const TUID);
        let wrapper_cid = vst3::iid(P::INFO.uid[0], P::INFO.uid[1], P::INFO.uid[2], P::INFO.uid[3]);
        if cid != wrapper_cid || iid != IComponent::IID {
            return result::INVALID_ARGUMENT;
        }

        let factory = &*(this as *const Factory<P>);

        let editor_context = Rc::new(Vst3EditorContext {
            alive: Cell::new(true),
            component_handler: Cell::new(ptr::null_mut()),
        });

        let (plugin, process_data, editor_data) = P::create(EditorContext(editor_context.clone()));

        *obj = Box::into_raw(Box::new(Wrapper {
            component: factory.component,
            audio_processor: factory.audio_processor,
            process_context_requirements: factory.process_context_requirements,
            edit_controller: factory.edit_controller,
            plug_view: factory.plug_view,
            event_handler: factory.event_handler,
            timer_handler: factory.timer_handler,
            count: AtomicU32::new(1),
            plugin,
            process_state: UnsafeCell::new(ProcessState {
                param_changes: Vec::with_capacity(P::PARAMS.len()),
                process_data,
            }),
            editor_state: UnsafeCell::new(EditorState {
                plug_frame: ptr::null_mut(),
                context: editor_context,
                editor_data,
            }),
        })) as *mut c_void;

        result::OK
    }

    pub unsafe extern "system" fn get_class_info_2(
        _this: *mut c_void,
        index: i32,
        info: *mut PClassInfo2,
    ) -> TResult {
        if index != 0 {
            return result::INVALID_ARGUMENT;
        }

        let info = &mut *info;

        info.cid = iid(P::INFO.uid[0], P::INFO.uid[1], P::INFO.uid[2], P::INFO.uid[3]);
        info.cardinality = PClassInfo::MANY_INSTANCES;
        copy_cstring("Audio Module Class", &mut info.category);
        copy_cstring(P::INFO.name, &mut info.name);
        info.class_flags = 0;
        copy_cstring("Fx", &mut info.sub_categories);
        copy_cstring(P::INFO.vendor, &mut info.vendor);
        copy_cstring("", &mut info.version);
        copy_cstring("VST 3.7", &mut info.sdk_version);

        result::OK
    }

    pub unsafe extern "system" fn get_class_info_unicode(
        _this: *mut c_void,
        index: i32,
        info: *mut PClassInfoW,
    ) -> TResult {
        if index != 0 {
            return result::INVALID_ARGUMENT;
        }

        let info = &mut *info;

        info.cid = iid(P::INFO.uid[0], P::INFO.uid[1], P::INFO.uid[2], P::INFO.uid[3]);
        info.cardinality = PClassInfo::MANY_INSTANCES;
        copy_cstring("Audio Module Class", &mut info.category);
        copy_wstring(P::INFO.name, &mut info.name);
        info.class_flags = 0;
        copy_cstring("Fx", &mut info.sub_categories);
        copy_wstring(P::INFO.vendor, &mut info.vendor);
        copy_wstring("", &mut info.version);
        copy_wstring("VST 3.7", &mut info.sdk_version);

        result::OK
    }

    pub unsafe extern "system" fn set_host_context(
        _this: *mut c_void,
        _context: *mut *const FUnknown,
    ) -> TResult {
        result::NOT_IMPLEMENTED
    }
}

struct Vst3EditorContext {
    alive: Cell<bool>,
    component_handler: Cell<*mut *const IComponentHandler>,
}

impl EditorContextInner for Vst3EditorContext {
    fn begin_edit(&self, param_id: ParamId) {
        let component_handler = self.component_handler.get();
        if self.alive.get() && !component_handler.is_null() {
            unsafe {
                ((*(*component_handler)).begin_edit)(component_handler as *mut c_void, param_id);
            }
        }
    }

    fn perform_edit(&self, param_id: ParamId, value: f64) {
        let component_handler = self.component_handler.get();
        if self.alive.get() && !component_handler.is_null() {
            unsafe {
                ((*(*component_handler)).perform_edit)(
                    component_handler as *mut c_void,
                    param_id,
                    value,
                );
            }
        }
    }

    fn end_edit(&self, param_id: ParamId) {
        let component_handler = self.component_handler.get();
        if self.alive.get() && !component_handler.is_null() {
            unsafe {
                ((*(*component_handler)).end_edit)(component_handler as *mut c_void, param_id);
            }
        }
    }
}

#[repr(C)]
pub struct Wrapper<P: Plugin> {
    component: *const IComponent,
    audio_processor: *const IAudioProcessor,
    process_context_requirements: *const IProcessContextRequirements,
    edit_controller: *const IEditController,
    plug_view: *const IPlugView,
    event_handler: *const IEventHandler,
    timer_handler: *const ITimerHandler,
    count: AtomicU32,
    plugin: P,
    process_state: UnsafeCell<ProcessState<P>>,
    editor_state: UnsafeCell<EditorState<P>>,
}

struct ProcessState<P: Plugin> {
    param_changes: Vec<ParamChange>,
    process_data: P::ProcessData,
}

struct EditorState<P: Plugin> {
    plug_frame: *mut *const IPlugFrame,
    context: Rc<Vst3EditorContext>,
    editor_data: P::EditorData,
}

unsafe impl<P: Plugin> Sync for Wrapper<P> {}

impl<P: Plugin> Wrapper<P> {
    const COMPONENT_OFFSET: isize = 0;
    const AUDIO_PROCESSOR_OFFSET: isize =
        Self::COMPONENT_OFFSET + mem::size_of::<*const IComponent>() as isize;
    const PROCESS_CONTEXT_REQUIREMENTS_OFFSET: isize =
        Self::AUDIO_PROCESSOR_OFFSET + mem::size_of::<*const IAudioProcessor>() as isize;
    const EDIT_CONTROLLER_OFFSET: isize = Self::PROCESS_CONTEXT_REQUIREMENTS_OFFSET
        + mem::size_of::<*const IProcessContextRequirements>() as isize;
    const PLUG_VIEW_OFFSET: isize =
        Self::EDIT_CONTROLLER_OFFSET + mem::size_of::<*const IEditController>() as isize;
    const EVENT_HANDLER_OFFSET: isize =
        Self::PLUG_VIEW_OFFSET + mem::size_of::<*const IPlugView>() as isize;
    const TIMER_HANDLER_OFFSET: isize =
        Self::EVENT_HANDLER_OFFSET + mem::size_of::<*const IEventHandler>() as isize;

    unsafe fn query_interface(
        this: *mut c_void,
        iid: *const TUID,
        obj: *mut *mut c_void,
    ) -> TResult {
        let iid = *iid;

        if iid == FUnknown::IID || iid == IComponent::IID {
            Self::add_ref(this);
            *obj = this.offset(Self::COMPONENT_OFFSET);
            return result::OK;
        }

        if iid == IAudioProcessor::IID {
            Self::add_ref(this);
            *obj = this.offset(Self::AUDIO_PROCESSOR_OFFSET);
            return result::OK;
        }

        if iid == IProcessContextRequirements::IID {
            Self::add_ref(this);
            *obj = this.offset(Self::PROCESS_CONTEXT_REQUIREMENTS_OFFSET);
            return result::OK;
        }

        if iid == IEditController::IID {
            Self::add_ref(this);
            *obj = this.offset(Self::EDIT_CONTROLLER_OFFSET);
            return result::OK;
        }

        if iid == IPlugView::IID && P::INFO.has_editor {
            Self::add_ref(this);
            *obj = this.offset(Self::PLUG_VIEW_OFFSET);
            return result::OK;
        }

        if iid == IEventHandler::IID {
            Self::add_ref(this);
            *obj = this.offset(Self::EVENT_HANDLER_OFFSET);
            return result::OK;
        }

        if iid == ITimerHandler::IID {
            Self::add_ref(this);
            *obj = this.offset(Self::TIMER_HANDLER_OFFSET);
            return result::OK;
        }

        result::NO_INTERFACE
    }

    unsafe fn add_ref(this: *mut c_void) -> u32 {
        (*(this as *const Wrapper<P>)).count.fetch_add(1, Ordering::Relaxed) + 1
    }

    unsafe fn release(this: *mut c_void) -> u32 {
        let count = (*(this as *const Wrapper<P>)).count.fetch_sub(1, Ordering::Release) - 1;

        if count == 0 {
            atomic::fence(Ordering::Acquire);
            drop(Box::from_raw(this as *mut Wrapper<P>));
        }

        count
    }

    pub unsafe extern "system" fn component_query_interface(
        this: *mut c_void,
        iid: *const TUID,
        obj: *mut *mut c_void,
    ) -> TResult {
        Self::query_interface(this.offset(-Self::COMPONENT_OFFSET), iid, obj)
    }

    pub unsafe extern "system" fn component_add_ref(this: *mut c_void) -> u32 {
        Self::add_ref(this.offset(-Self::COMPONENT_OFFSET))
    }

    pub unsafe extern "system" fn component_release(this: *mut c_void) -> u32 {
        Self::release(this.offset(-Self::COMPONENT_OFFSET))
    }

    pub unsafe extern "system" fn component_initialize(
        _this: *mut c_void,
        _context: *mut FUnknown,
    ) -> TResult {
        result::OK
    }

    pub unsafe extern "system" fn component_terminate(_this: *mut c_void) -> TResult {
        result::OK
    }

    pub unsafe extern "system" fn get_controller_class_id(
        _this: *mut c_void,
        _class_id: *const TUID,
    ) -> TResult {
        result::NOT_IMPLEMENTED
    }

    pub unsafe extern "system" fn set_io_mode(_this: *mut c_void, _mode: IoMode) -> TResult {
        result::OK
    }

    pub unsafe extern "system" fn get_bus_count(
        _this: *mut c_void,
        media_type: MediaType,
        dir: BusDirection,
    ) -> i32 {
        match media_type {
            media_types::AUDIO => match dir {
                bus_directions::INPUT => 1,
                bus_directions::OUTPUT => 1,
                _ => 0,
            },
            media_types::EVENT => 0,
            _ => 0,
        }
    }

    pub unsafe extern "system" fn get_bus_info(
        _this: *mut c_void,
        media_type: MediaType,
        dir: BusDirection,
        index: i32,
        bus: *mut BusInfo,
    ) -> TResult {
        match media_type {
            media_types::AUDIO => match dir {
                bus_directions::INPUT => match index {
                    0 => {
                        let bus = &mut *bus;

                        bus.media_type = media_types::AUDIO;
                        bus.direction = bus_directions::INPUT;
                        bus.channel_count = 2;
                        copy_wstring("input", &mut bus.name);
                        bus.bus_type = bus_types::MAIN;
                        bus.flags = BusInfo::DEFAULT_ACTIVE;

                        result::OK
                    }
                    _ => result::INVALID_ARGUMENT,
                },
                bus_directions::OUTPUT => match index {
                    0 => {
                        let bus = &mut *bus;

                        bus.media_type = media_types::AUDIO;
                        bus.direction = bus_directions::OUTPUT;
                        bus.channel_count = 2;
                        copy_wstring("output", &mut bus.name);
                        bus.bus_type = bus_types::MAIN;
                        bus.flags = BusInfo::DEFAULT_ACTIVE;

                        result::OK
                    }
                    _ => result::INVALID_ARGUMENT,
                },
                _ => result::INVALID_ARGUMENT,
            },
            media_types::EVENT => result::INVALID_ARGUMENT,
            _ => result::INVALID_ARGUMENT,
        }
    }

    pub unsafe extern "system" fn get_routing_info(
        _this: *mut c_void,
        _in_info: *mut RoutingInfo,
        _out_info: *mut RoutingInfo,
    ) -> TResult {
        result::NOT_IMPLEMENTED
    }

    pub unsafe extern "system" fn activate_bus(
        _this: *mut c_void,
        _media_type: MediaType,
        _dir: BusDirection,
        _index: i32,
        _state: TBool,
    ) -> TResult {
        result::OK
    }

    pub unsafe extern "system" fn set_active(_this: *mut c_void, _state: TBool) -> TResult {
        result::OK
    }

    pub unsafe extern "system" fn component_set_state(
        _this: *mut c_void,
        _state: *mut IBStream,
    ) -> TResult {
        result::OK
    }

    pub unsafe extern "system" fn component_get_state(
        _this: *mut c_void,
        _state: *mut IBStream,
    ) -> TResult {
        result::OK
    }

    pub unsafe extern "system" fn audio_processor_query_interface(
        this: *mut c_void,
        iid: *const TUID,
        obj: *mut *mut c_void,
    ) -> TResult {
        Self::query_interface(this.offset(-Self::AUDIO_PROCESSOR_OFFSET), iid, obj)
    }

    pub unsafe extern "system" fn audio_processor_add_ref(this: *mut c_void) -> u32 {
        Self::add_ref(this.offset(-Self::AUDIO_PROCESSOR_OFFSET))
    }

    pub unsafe extern "system" fn audio_processor_release(this: *mut c_void) -> u32 {
        Self::release(this.offset(-Self::AUDIO_PROCESSOR_OFFSET))
    }

    pub unsafe extern "system" fn set_bus_arrangements(
        _this: *mut c_void,
        inputs: *const SpeakerArrangement,
        num_ins: i32,
        outputs: *const SpeakerArrangement,
        num_outs: i32,
    ) -> TResult {
        if num_ins != 1 || num_outs != 1 {
            return result::FALSE;
        }

        if *inputs != speaker_arrangements::STEREO || *outputs != speaker_arrangements::STEREO {
            return result::FALSE;
        }

        result::TRUE
    }

    pub unsafe extern "system" fn get_bus_arrangement(
        _this: *mut c_void,
        dir: BusDirection,
        index: i32,
        arr: *mut SpeakerArrangement,
    ) -> TResult {
        match dir {
            bus_directions::INPUT => {
                if index == 0 {
                    *arr = speaker_arrangements::STEREO;
                    result::OK
                } else {
                    result::INVALID_ARGUMENT
                }
            }
            bus_directions::OUTPUT => {
                if index == 0 {
                    *arr = speaker_arrangements::STEREO;
                    result::OK
                } else {
                    result::INVALID_ARGUMENT
                }
            }
            _ => result::INVALID_ARGUMENT,
        }
    }

    pub unsafe extern "system" fn can_process_sample_size(
        _this: *mut c_void,
        symbolic_sample_size: i32,
    ) -> TResult {
        match symbolic_sample_size {
            symbolic_sample_sizes::SAMPLE_32 => result::OK,
            symbolic_sample_sizes::SAMPLE_64 => result::NOT_IMPLEMENTED,
            _ => result::INVALID_ARGUMENT,
        }
    }

    pub unsafe extern "system" fn get_latency_samples(_this: *mut c_void) -> u32 {
        0
    }

    pub unsafe extern "system" fn setup_processing(
        _this: *mut c_void,
        _setup: *mut ProcessSetup,
    ) -> TResult {
        result::OK
    }

    pub unsafe extern "system" fn set_processing(_this: *mut c_void, _state: TBool) -> TResult {
        result::OK
    }

    pub unsafe extern "system" fn process(this: *mut c_void, data: *mut ProcessData) -> TResult {
        let wrapper = &*(this.offset(-Self::AUDIO_PROCESSOR_OFFSET) as *const Wrapper<P>);
        let process_state = &mut *wrapper.process_state.get();

        process_state.param_changes.clear();

        let process_data = &*data;

        let param_changes = process_data.input_parameter_changes;
        if !param_changes.is_null() {
            let param_count =
                ((*(*param_changes)).get_parameter_count)(param_changes as *mut c_void);
            for param_index in 0..param_count {
                let param_data = ((*(*param_changes)).get_parameter_data)(
                    param_changes as *mut c_void,
                    param_index,
                );

                if param_data.is_null() {
                    continue;
                }

                let param_id = ((*(*param_data)).get_parameter_id)(param_data as *mut c_void);
                let point_count = ((*(*param_data)).get_point_count)(param_data as *mut c_void);

                if point_count <= 0 {
                    continue;
                }

                let mut offset = 0;
                let mut value = 0.0;
                let result = ((*(*param_data)).get_point)(
                    param_data as *mut c_void,
                    point_count - 1,
                    &mut offset,
                    &mut value,
                );

                if result != result::OK {
                    continue;
                }

                process_state.param_changes.push(ParamChange { id: param_id, offset: 0, value });
            }
        }

        if process_data.num_inputs != 1 || process_data.num_outputs != 1 {
            return result::OK;
        }

        let input_bus = &*process_data.inputs;
        let output_bus = &*process_data.outputs;

        if input_bus.num_channels != 2 || output_bus.num_channels != 2 {
            return result::OK;
        }

        let input_ptrs =
            slice::from_raw_parts(input_bus.channel_buffers, input_bus.num_channels as usize);
        let output_ptrs =
            slice::from_raw_parts(output_bus.channel_buffers, output_bus.num_channels as usize);

        if input_ptrs[0].is_null()
            || input_ptrs[1].is_null()
            || output_ptrs[0].is_null()
            || output_ptrs[1].is_null()
        {
            return result::OK;
        }

        let input_slices = &[
            slice::from_raw_parts(input_ptrs[0] as *const f32, process_data.num_samples as usize),
            slice::from_raw_parts(input_ptrs[1] as *const f32, process_data.num_samples as usize),
        ];

        let output_slices = &mut [
            slice::from_raw_parts_mut(
                output_ptrs[0] as *mut f32,
                process_data.num_samples as usize,
            ),
            slice::from_raw_parts_mut(
                output_ptrs[1] as *mut f32,
                process_data.num_samples as usize,
            ),
        ];

        wrapper.plugin.process(
            &mut process_state.process_data,
            input_slices,
            output_slices,
            &process_state.param_changes[..],
        );

        result::OK
    }

    pub unsafe extern "system" fn get_tail_samples(_this: *mut c_void) -> u32 {
        0
    }

    pub unsafe extern "system" fn process_context_requirements_query_interface(
        this: *mut c_void,
        iid: *const TUID,
        obj: *mut *mut c_void,
    ) -> TResult {
        Self::query_interface(this.offset(-Self::PROCESS_CONTEXT_REQUIREMENTS_OFFSET), iid, obj)
    }

    pub unsafe extern "system" fn process_context_requirements_add_ref(this: *mut c_void) -> u32 {
        Self::add_ref(this.offset(-Self::PROCESS_CONTEXT_REQUIREMENTS_OFFSET))
    }

    pub unsafe extern "system" fn process_context_requirements_release(this: *mut c_void) -> u32 {
        Self::release(this.offset(-Self::PROCESS_CONTEXT_REQUIREMENTS_OFFSET))
    }

    pub unsafe extern "system" fn get_process_context_requirements(_this: *mut c_void) -> u32 {
        0
    }

    pub unsafe extern "system" fn edit_controller_query_interface(
        this: *mut c_void,
        iid: *const TUID,
        obj: *mut *mut c_void,
    ) -> TResult {
        Self::query_interface(this.offset(-Self::EDIT_CONTROLLER_OFFSET), iid, obj)
    }

    pub unsafe extern "system" fn edit_controller_add_ref(this: *mut c_void) -> u32 {
        Self::add_ref(this.offset(-Self::EDIT_CONTROLLER_OFFSET))
    }

    pub unsafe extern "system" fn edit_controller_release(this: *mut c_void) -> u32 {
        Self::release(this.offset(-Self::EDIT_CONTROLLER_OFFSET))
    }

    pub unsafe extern "system" fn edit_controller_initialize(
        _this: *mut c_void,
        _context: *mut FUnknown,
    ) -> TResult {
        result::OK
    }

    pub unsafe extern "system" fn edit_controller_terminate(this: *mut c_void) -> TResult {
        let wrapper = &*(this.offset(-Self::EDIT_CONTROLLER_OFFSET) as *const Wrapper<P>);
        let editor_state = &mut *wrapper.editor_state.get();

        editor_state.context.alive.set(false);
        let component_handler = editor_state.context.component_handler.get();
        if !component_handler.is_null() {
            ((*(*component_handler)).unknown.release)(component_handler as *mut c_void);
            editor_state.context.component_handler.set(ptr::null_mut());
        }

        if !editor_state.plug_frame.is_null() {
            ((*(*editor_state.plug_frame)).unknown.release)(editor_state.plug_frame as *mut c_void);
            editor_state.plug_frame = ptr::null_mut();
        }

        result::OK
    }

    pub unsafe extern "system" fn set_component_state(
        _this: *mut c_void,
        _state: *mut *const IBStream,
    ) -> TResult {
        result::NOT_IMPLEMENTED
    }

    pub unsafe extern "system" fn edit_controller_set_state(
        _this: *mut c_void,
        _state: *mut *const IBStream,
    ) -> TResult {
        result::OK
    }

    pub unsafe extern "system" fn edit_controller_get_state(
        _this: *mut c_void,
        _state: *mut *const IBStream,
    ) -> TResult {
        result::OK
    }

    pub unsafe extern "system" fn get_parameter_count(_this: *mut c_void) -> i32 {
        P::PARAMS.len() as i32
    }

    pub unsafe extern "system" fn get_parameter_info(
        _this: *mut c_void,
        param_index: i32,
        info: *mut ParameterInfo,
    ) -> TResult {
        if let Some(param_info) = P::PARAMS.get(param_index as usize) {
            let info = &mut *info;

            info.id = param_index as u32;
            copy_wstring(param_info.name, &mut info.title);
            copy_wstring(param_info.name, &mut info.short_title);
            copy_wstring(param_info.label, &mut info.units);
            info.step_count = param_info.steps.unwrap_or(0) as i32;
            info.default_normalized_value = (param_info.to_normal)(param_info.default);
            info.unit_id = 0;
            info.flags = ParameterInfo::CAN_AUTOMATE;

            result::OK
        } else {
            result::INVALID_ARGUMENT
        }
    }

    pub unsafe extern "system" fn get_param_string_by_value(
        _this: *mut c_void,
        id: u32,
        value_normalized: f64,
        string: *mut String128,
    ) -> TResult {
        if let Some(param_info) = P::PARAMS.get(id as usize) {
            let display = (param_info.to_string)((param_info.from_normal)(value_normalized));
            copy_wstring(&display, &mut *string);
            result::OK
        } else {
            result::INVALID_ARGUMENT
        }
    }

    pub unsafe extern "system" fn get_param_value_by_string(
        _this: *mut c_void,
        id: u32,
        string: *const TChar,
        value_normalized: *mut f64,
    ) -> TResult {
        let len = len_wstring(string);
        let string = String::from_utf16(slice::from_raw_parts(string as *const u16, len));

        let param_info = P::PARAMS.get(id as usize);

        if let (Ok(string), Some(param_info)) = (string, param_info) {
            *value_normalized = (param_info.from_string)(&string);
            result::OK
        } else {
            result::INVALID_ARGUMENT
        }
    }

    pub unsafe extern "system" fn normalized_param_to_plain(
        _this: *mut c_void,
        id: u32,
        value_normalized: f64,
    ) -> f64 {
        if let Some(param_info) = P::PARAMS.get(id as usize) {
            (param_info.from_normal)(value_normalized)
        } else {
            0.0
        }
    }

    pub unsafe extern "system" fn plain_param_to_normalized(
        _this: *mut c_void,
        id: u32,
        plain_value: f64,
    ) -> f64 {
        if let Some(param_info) = P::PARAMS.get(id as usize) {
            (param_info.to_normal)(plain_value)
        } else {
            0.0
        }
    }

    pub unsafe extern "system" fn get_param_normalized(this: *mut c_void, id: u32) -> f64 {
        let wrapper = &*(this.offset(-Self::EDIT_CONTROLLER_OFFSET) as *const Wrapper<P>);

        wrapper.plugin.get_param(id)
    }

    pub unsafe extern "system" fn set_param_normalized(
        this: *mut c_void,
        id: u32,
        value: f64,
    ) -> TResult {
        let wrapper = &*(this.offset(-Self::EDIT_CONTROLLER_OFFSET) as *const Wrapper<P>);

        wrapper.plugin.set_param(id, value);

        result::OK
    }

    pub unsafe extern "system" fn set_component_handler(
        this: *mut c_void,
        handler: *mut *const IComponentHandler,
    ) -> TResult {
        let wrapper = &*(this.offset(-Self::EDIT_CONTROLLER_OFFSET) as *const Wrapper<P>);
        let editor_state = &*wrapper.editor_state.get();

        if !handler.is_null() {
            ((*(*handler)).unknown.add_ref)(handler as *mut c_void);
        }
        editor_state.context.component_handler.set(handler);

        result::OK
    }

    pub unsafe extern "system" fn create_view(
        this: *mut c_void,
        name: *const c_char,
    ) -> *mut *const IPlugView {
        if !P::INFO.has_editor {
            return ptr::null_mut();
        }

        if ffi::CStr::from_ptr(name) == ffi::CStr::from_ptr(view_types::EDITOR) {
            Self::add_ref(this.offset(-Self::EDIT_CONTROLLER_OFFSET));
            return this.offset(-Self::EDIT_CONTROLLER_OFFSET + Self::PLUG_VIEW_OFFSET)
                as *mut *const IPlugView;
        }

        ptr::null_mut()
    }

    pub unsafe extern "system" fn plug_view_query_interface(
        this: *mut c_void,
        iid: *const TUID,
        obj: *mut *mut c_void,
    ) -> TResult {
        Self::query_interface(this.offset(-Self::PLUG_VIEW_OFFSET), iid, obj)
    }

    pub unsafe extern "system" fn plug_view_add_ref(this: *mut c_void) -> u32 {
        Self::add_ref(this.offset(-Self::PLUG_VIEW_OFFSET))
    }

    pub unsafe extern "system" fn plug_view_release(this: *mut c_void) -> u32 {
        Self::release(this.offset(-Self::PLUG_VIEW_OFFSET))
    }

    pub unsafe extern "system" fn is_platform_type_supported(
        _this: *mut c_void,
        platform_type: *const c_char,
    ) -> TResult {
        #[cfg(target_os = "windows")]
        if ffi::CStr::from_ptr(platform_type) == ffi::CStr::from_ptr(platform_types::HWND) {
            return result::TRUE;
        }

        #[cfg(target_os = "macos")]
        if ffi::CStr::from_ptr(platform_type) == ffi::CStr::from_ptr(platform_types::NS_VIEW) {
            return result::TRUE;
        }

        #[cfg(target_os = "linux")]
        if ffi::CStr::from_ptr(platform_type)
            == ffi::CStr::from_ptr(platform_types::X11_EMBED_WINDOW_ID)
        {
            return result::TRUE;
        }

        result::FALSE
    }

    pub unsafe extern "system" fn attached(
        this: *mut c_void,
        parent: *mut c_void,
        platform_type: *const c_char,
    ) -> TResult {
        if Self::is_platform_type_supported(this, platform_type) != result::TRUE {
            return result::NOT_IMPLEMENTED;
        }

        let wrapper = &*(this.offset(-Self::PLUG_VIEW_OFFSET) as *const Wrapper<P>);
        let editor_state = &mut *wrapper.editor_state.get();

        #[cfg(target_os = "macos")]
        let parent = {
            use raw_window_handle::macos::MacOSHandle;
            RawWindowHandle::MacOS(MacOSHandle { ns_view: parent, ..MacOSHandle::empty() })
        };

        #[cfg(target_os = "windows")]
        let parent = {
            use raw_window_handle::windows::WindowsHandle;
            RawWindowHandle::Windows(WindowsHandle { hwnd: parent, ..WindowsHandle::empty() })
        };

        #[cfg(target_os = "linux")]
        let parent = {
            use raw_window_handle::unix::XcbHandle;
            RawWindowHandle::Xcb(XcbHandle { window: parent as u32, ..XcbHandle::empty() })
        };

        wrapper.plugin.editor_open(&mut editor_state.editor_data, Some(&ParentWindow(parent)));

        #[cfg(target_os = "linux")]
        if let Some(file_descriptor) = wrapper.plugin.file_descriptor(&mut editor_state.editor_data) {
            if !editor_state.plug_frame.is_null() {
                let mut obj = ptr::null_mut();
                let result = ((*(*editor_state.plug_frame)).unknown.query_interface)(
                    editor_state.plug_frame as *mut c_void,
                    &IRunLoop::IID,
                    &mut obj,
                );
                if result == result::OK {
                    let run_loop = obj as *mut *const IRunLoop;

                    let event_handler = this
                        .offset(-Self::PLUG_VIEW_OFFSET + Self::EVENT_HANDLER_OFFSET)
                        as *mut *const IEventHandler;
                    ((*(*run_loop)).register_event_handler)(
                        run_loop as *mut c_void,
                        event_handler,
                        file_descriptor,
                    );

                    let timer_handler = this
                        .offset(-Self::PLUG_VIEW_OFFSET + Self::TIMER_HANDLER_OFFSET)
                        as *mut *const ITimerHandler;
                    ((*(*run_loop)).register_timer)(run_loop as *mut c_void, timer_handler, 16);

                    ((*(*run_loop)).unknown.release)(run_loop as *mut c_void);
                }
            }
        }

        result::OK
    }

    pub unsafe extern "system" fn removed(this: *mut c_void) -> TResult {
        let wrapper = &*(this.offset(-Self::PLUG_VIEW_OFFSET) as *const Wrapper<P>);
        let editor_state = &mut *wrapper.editor_state.get();

        wrapper.plugin.editor_close(&mut editor_state.editor_data);

        #[cfg(target_os = "linux")]
        {
            if !editor_state.plug_frame.is_null() {
                let mut obj = ptr::null_mut();
                let result = ((*(*editor_state.plug_frame)).unknown.query_interface)(
                    editor_state.plug_frame as *mut c_void,
                    &IRunLoop::IID,
                    &mut obj,
                );
                if result == result::OK {
                    let run_loop = obj as *mut *const IRunLoop;

                    let event_handler = this
                        .offset(-Self::PLUG_VIEW_OFFSET + Self::EVENT_HANDLER_OFFSET)
                        as *mut *const IEventHandler;
                    ((*(*run_loop)).unregister_event_handler)(
                        run_loop as *mut c_void,
                        event_handler,
                    );

                    let timer_handler = this
                        .offset(-Self::PLUG_VIEW_OFFSET + Self::TIMER_HANDLER_OFFSET)
                        as *mut *const ITimerHandler;
                    ((*(*run_loop)).unregister_timer)(run_loop as *mut c_void, timer_handler);

                    ((*(*run_loop)).unknown.release)(run_loop as *mut c_void);
                }
            }
        }

        result::OK
    }

    pub unsafe extern "system" fn on_wheel(_this: *mut c_void, _distance: f32) -> TResult {
        result::NOT_IMPLEMENTED
    }

    pub unsafe extern "system" fn on_key_down(
        _this: *mut c_void,
        _key: i16,
        _key_code: i16,
        _modifiers: i16,
    ) -> TResult {
        result::NOT_IMPLEMENTED
    }

    pub unsafe extern "system" fn on_key_up(
        _this: *mut c_void,
        _key: i16,
        _key_code: i16,
        _modifiers: i16,
    ) -> TResult {
        result::NOT_IMPLEMENTED
    }

    pub unsafe extern "system" fn get_size(this: *mut c_void, size: *mut ViewRect) -> TResult {
        let wrapper = &*(this.offset(-Self::PLUG_VIEW_OFFSET) as *const Wrapper<P>);
        let editor_state = &*wrapper.editor_state.get();

        let (width, height) = wrapper.plugin.editor_size(&editor_state.editor_data);

        let size = &mut *size;
        size.left = 0;
        size.top = 0;
        size.right = width.round() as i32;
        size.bottom = height.round() as i32;

        result::OK
    }

    pub unsafe extern "system" fn on_size(
        _this: *mut c_void,
        _new_size: *const ViewRect,
    ) -> TResult {
        result::NOT_IMPLEMENTED
    }

    pub unsafe extern "system" fn on_focus(_this: *mut c_void, _state: TBool) -> TResult {
        result::NOT_IMPLEMENTED
    }

    pub unsafe extern "system" fn set_frame(
        this: *mut c_void,
        frame: *mut *const IPlugFrame,
    ) -> TResult {
        let wrapper = &*(this.offset(-Self::PLUG_VIEW_OFFSET) as *const Wrapper<P>);
        let editor_state = &mut *wrapper.editor_state.get();

        if !editor_state.plug_frame.is_null() {
            ((*(*editor_state.plug_frame)).unknown.release)(editor_state.plug_frame as *mut c_void);
        }
        if !frame.is_null() {
            ((*(*frame)).unknown.add_ref)(frame as *mut c_void);
        }
        editor_state.plug_frame = frame;

        result::OK
    }

    pub unsafe extern "system" fn can_resize(_this: *mut c_void) -> TResult {
        result::FALSE
    }

    pub unsafe extern "system" fn check_size_constraint(
        _this: *mut c_void,
        _rect: *mut ViewRect,
    ) -> TResult {
        result::NOT_IMPLEMENTED
    }

    pub unsafe extern "system" fn event_handler_query_interface(
        this: *mut c_void,
        iid: *const TUID,
        obj: *mut *mut c_void,
    ) -> TResult {
        Self::query_interface(this.offset(-Self::EVENT_HANDLER_OFFSET), iid, obj)
    }

    pub unsafe extern "system" fn event_handler_add_ref(this: *mut c_void) -> u32 {
        Self::add_ref(this.offset(-Self::EVENT_HANDLER_OFFSET))
    }

    pub unsafe extern "system" fn event_handler_release(this: *mut c_void) -> u32 {
        Self::release(this.offset(-Self::EVENT_HANDLER_OFFSET))
    }

    pub unsafe extern "system" fn on_fd_is_set(this: *mut c_void, _fd: c_int) {
        let wrapper = &*(this.offset(-Self::EVENT_HANDLER_OFFSET) as *const Wrapper<P>);
        let editor_state = &mut *wrapper.editor_state.get();

        wrapper.plugin.editor_poll(&mut editor_state.editor_data);
    }

    pub unsafe extern "system" fn timer_handler_query_interface(
        this: *mut c_void,
        iid: *const TUID,
        obj: *mut *mut c_void,
    ) -> TResult {
        Self::query_interface(this.offset(-Self::TIMER_HANDLER_OFFSET), iid, obj)
    }

    pub unsafe extern "system" fn timer_handler_add_ref(this: *mut c_void) -> u32 {
        Self::add_ref(this.offset(-Self::TIMER_HANDLER_OFFSET))
    }

    pub unsafe extern "system" fn timer_handler_release(this: *mut c_void) -> u32 {
        Self::release(this.offset(-Self::TIMER_HANDLER_OFFSET))
    }

    pub unsafe extern "system" fn on_timer(this: *mut c_void) {
        let wrapper = &*(this.offset(-Self::TIMER_HANDLER_OFFSET) as *const Wrapper<P>);
        let editor_state = &mut *wrapper.editor_state.get();

        wrapper.plugin.editor_poll(&mut editor_state.editor_data);
    }
}

#[macro_export]
macro_rules! vst3 {
    ($plugin:ty) => {
        mod vst3_impl {
            use std::ffi::c_void;
            use std::marker::PhantomData;

            use $crate::vst3::vst3_api::*;
            use $crate::vst3::*;

            static PLUGIN_FACTORY_3_VTABLE: IPluginFactory3 = IPluginFactory3 {
                plugin_factory_2: IPluginFactory2 {
                    plugin_factory: IPluginFactory {
                        unknown: FUnknown {
                            query_interface: Factory::<$plugin>::query_interface,
                            add_ref: Factory::<$plugin>::add_ref,
                            release: Factory::<$plugin>::release,
                        },
                        get_factory_info: Factory::<$plugin>::get_factory_info,
                        count_classes: Factory::<$plugin>::count_classes,
                        get_class_info: Factory::<$plugin>::get_class_info,
                        create_instance: Factory::<$plugin>::create_instance,
                    },
                    get_class_info_2: Factory::<$plugin>::get_class_info_2,
                },
                get_class_info_unicode: Factory::<$plugin>::get_class_info_unicode,
                set_host_context: Factory::<$plugin>::set_host_context,
            };

            static COMPONENT_VTABLE: IComponent = IComponent {
                plugin_base: IPluginBase {
                    unknown: FUnknown {
                        query_interface: Wrapper::<$plugin>::component_query_interface,
                        add_ref: Wrapper::<$plugin>::component_add_ref,
                        release: Wrapper::<$plugin>::component_release,
                    },
                    initialize: Wrapper::<$plugin>::component_initialize,
                    terminate: Wrapper::<$plugin>::component_terminate,
                },
                get_controller_class_id: Wrapper::<$plugin>::get_controller_class_id,
                set_io_mode: Wrapper::<$plugin>::set_io_mode,
                get_bus_count: Wrapper::<$plugin>::get_bus_count,
                get_bus_info: Wrapper::<$plugin>::get_bus_info,
                get_routing_info: Wrapper::<$plugin>::get_routing_info,
                activate_bus: Wrapper::<$plugin>::activate_bus,
                set_active: Wrapper::<$plugin>::set_active,
                set_state: Wrapper::<$plugin>::component_set_state,
                get_state: Wrapper::<$plugin>::component_get_state,
            };

            static AUDIO_PROCESSOR_VTABLE: IAudioProcessor = IAudioProcessor {
                unknown: FUnknown {
                    query_interface: Wrapper::<$plugin>::audio_processor_query_interface,
                    add_ref: Wrapper::<$plugin>::audio_processor_add_ref,
                    release: Wrapper::<$plugin>::audio_processor_release,
                },
                set_bus_arrangements: Wrapper::<$plugin>::set_bus_arrangements,
                get_bus_arrangement: Wrapper::<$plugin>::get_bus_arrangement,
                can_process_sample_size: Wrapper::<$plugin>::can_process_sample_size,
                get_latency_samples: Wrapper::<$plugin>::get_latency_samples,
                setup_processing: Wrapper::<$plugin>::setup_processing,
                set_processing: Wrapper::<$plugin>::set_processing,
                process: Wrapper::<$plugin>::process,
                get_tail_samples: Wrapper::<$plugin>::get_tail_samples,
            };

            static PROCESS_CONTEXT_REQUIREMENTS_VTABLE: IProcessContextRequirements =
                IProcessContextRequirements {
                    unknown: FUnknown {
                        query_interface:
                            Wrapper::<$plugin>::process_context_requirements_query_interface,
                        add_ref: Wrapper::<$plugin>::process_context_requirements_add_ref,
                        release: Wrapper::<$plugin>::process_context_requirements_release,
                    },
                    get_process_context_requirements:
                        Wrapper::<$plugin>::get_process_context_requirements,
                };

            static EDIT_CONTROLLER_VTABLE: IEditController = IEditController {
                plugin_base: IPluginBase {
                    unknown: FUnknown {
                        query_interface: Wrapper::<$plugin>::edit_controller_query_interface,
                        add_ref: Wrapper::<$plugin>::edit_controller_add_ref,
                        release: Wrapper::<$plugin>::edit_controller_release,
                    },
                    initialize: Wrapper::<$plugin>::edit_controller_initialize,
                    terminate: Wrapper::<$plugin>::edit_controller_terminate,
                },
                set_component_state: Wrapper::<$plugin>::set_component_state,
                set_state: Wrapper::<$plugin>::edit_controller_set_state,
                get_state: Wrapper::<$plugin>::edit_controller_get_state,
                get_parameter_count: Wrapper::<$plugin>::get_parameter_count,
                get_parameter_info: Wrapper::<$plugin>::get_parameter_info,
                get_param_string_by_value: Wrapper::<$plugin>::get_param_string_by_value,
                get_param_value_by_string: Wrapper::<$plugin>::get_param_value_by_string,
                normalized_param_to_plain: Wrapper::<$plugin>::normalized_param_to_plain,
                plain_param_to_normalized: Wrapper::<$plugin>::plain_param_to_normalized,
                get_param_normalized: Wrapper::<$plugin>::get_param_normalized,
                set_param_normalized: Wrapper::<$plugin>::set_param_normalized,
                set_component_handler: Wrapper::<$plugin>::set_component_handler,
                create_view: Wrapper::<$plugin>::create_view,
            };

            static PLUG_VIEW_VTABLE: IPlugView = IPlugView {
                unknown: FUnknown {
                    query_interface: Wrapper::<$plugin>::plug_view_query_interface,
                    add_ref: Wrapper::<$plugin>::plug_view_add_ref,
                    release: Wrapper::<$plugin>::plug_view_release,
                },
                is_platform_type_supported: Wrapper::<$plugin>::is_platform_type_supported,
                attached: Wrapper::<$plugin>::attached,
                removed: Wrapper::<$plugin>::removed,
                on_wheel: Wrapper::<$plugin>::on_wheel,
                on_key_down: Wrapper::<$plugin>::on_key_down,
                on_key_up: Wrapper::<$plugin>::on_key_up,
                get_size: Wrapper::<$plugin>::get_size,
                on_size: Wrapper::<$plugin>::on_size,
                on_focus: Wrapper::<$plugin>::on_focus,
                set_frame: Wrapper::<$plugin>::set_frame,
                can_resize: Wrapper::<$plugin>::can_resize,
                check_size_constraint: Wrapper::<$plugin>::check_size_constraint,
            };

            static EVENT_HANDLER_VTABLE: IEventHandler = IEventHandler {
                unknown: FUnknown {
                    query_interface: Wrapper::<$plugin>::event_handler_query_interface,
                    add_ref: Wrapper::<$plugin>::event_handler_add_ref,
                    release: Wrapper::<$plugin>::event_handler_release,
                },
                on_fd_is_set: Wrapper::<$plugin>::on_fd_is_set,
            };

            static TIMER_HANDLER_VTABLE: ITimerHandler = ITimerHandler {
                unknown: FUnknown {
                    query_interface: Wrapper::<$plugin>::timer_handler_query_interface,
                    add_ref: Wrapper::<$plugin>::timer_handler_add_ref,
                    release: Wrapper::<$plugin>::timer_handler_release,
                },
                on_timer: Wrapper::<$plugin>::on_timer,
            };

            static PLUGIN_FACTORY: Factory<$plugin> = Factory {
                plugin_factory_3: &PLUGIN_FACTORY_3_VTABLE,
                component: &COMPONENT_VTABLE,
                audio_processor: &AUDIO_PROCESSOR_VTABLE,
                process_context_requirements: &PROCESS_CONTEXT_REQUIREMENTS_VTABLE,
                edit_controller: &EDIT_CONTROLLER_VTABLE,
                plug_view: &PLUG_VIEW_VTABLE,
                event_handler: &EVENT_HANDLER_VTABLE,
                timer_handler: &TIMER_HANDLER_VTABLE,
                phantom: PhantomData,
            };

            #[cfg(target_os = "windows")]
            #[no_mangle]
            extern "system" fn InitDll() -> bool {
                true
            }

            #[cfg(target_os = "windows")]
            #[no_mangle]
            extern "system" fn ExitDll() -> bool {
                true
            }

            #[cfg(target_os = "macos")]
            #[no_mangle]
            extern "system" fn BundleEntry(_bundle_ref: *mut c_void) -> bool {
                true
            }

            #[cfg(target_os = "macos")]
            #[no_mangle]
            extern "system" fn BundleExit() -> bool {
                true
            }

            #[cfg(target_os = "linux")]
            #[no_mangle]
            extern "system" fn ModuleEntry(_library_handle: *mut c_void) -> bool {
                true
            }

            #[cfg(target_os = "linux")]
            #[no_mangle]
            extern "system" fn ModuleExit() -> bool {
                true
            }

            #[no_mangle]
            extern "system" fn GetPluginFactory() -> *mut c_void {
                &PLUGIN_FACTORY as *const Factory<$plugin> as *mut c_void
            }
        }
    };
}
