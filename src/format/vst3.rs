#![allow(non_snake_case)]

use std::cell::{RefCell, UnsafeCell};
use std::collections::HashSet;
use std::ffi::{c_void, CStr};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::{io, ptr, slice};

use raw_window_handle::RawWindowHandle;

use vst3_bindgen::{uid, Class, ComPtr, ComRef, ComWrapper, Steinberg::Vst::*, Steinberg::*};

use super::util::{self, copy_cstring};
use crate::atomic::AtomicBitset;
use crate::buffer::Buffers;
use crate::bus::{BusConfig, BusFormat, BusList, BusState};
use crate::editor::{Editor, EditorContext, EditorContextHandler, ParentWindow, PollParams};
use crate::param::ParamId;
use crate::plugin::{Plugin, PluginHandle, PluginInfo};
use crate::process::{Event, EventType, ParamChange, ProcessContext, Processor};

fn copy_wstring(src: &str, dst: &mut [TChar]) {
    let mut len = 0;
    for (src, dst) in src.encode_utf16().zip(dst.iter_mut()) {
        *dst = src as TChar;
        len += 1;
    }

    if len < dst.len() {
        dst[len] = 0;
    } else if let Some(last) = dst.last_mut() {
        *last = 0;
    }
}

unsafe fn len_wstring(string: *const i16) -> usize {
    let mut len = 0;

    while *string.offset(len) != 0 {
        len += 1;
    }

    len as usize
}

fn bus_format_to_speaker_arrangement(bus_format: &BusFormat) -> SpeakerArrangement {
    match bus_format {
        BusFormat::Stereo => SpeakerArr::kStereo,
    }
}

fn speaker_arrangement_to_bus_format(speaker_arrangement: SpeakerArrangement) -> Option<BusFormat> {
    match speaker_arrangement {
        SpeakerArr::kStereo => Some(BusFormat::Stereo),
        _ => None,
    }
}

struct Vst3EditorContext<P> {
    component_handler: RefCell<Option<ComPtr<IComponentHandler>>>,
    plug_frame: RefCell<Option<ComPtr<IPlugFrame>>>,
    plugin: PluginHandle<P>,
    param_states: Arc<ParamStates>,
}

impl<P> EditorContextHandler<P> for Vst3EditorContext<P> {
    fn begin_edit(&self, id: ParamId) {
        let _ = PluginHandle::params(&self.plugin)
            .index_of(id)
            .expect("Invalid parameter id");

        let component_handler = self.component_handler.borrow().clone();
        if let Some(component_handler) = &component_handler {
            unsafe {
                component_handler.beginEdit(id);
            }
        }
    }

    fn perform_edit(&self, id: ParamId, value: f64) {
        let param_index = PluginHandle::params(&self.plugin)
            .index_of(id)
            .expect("Invalid parameter id");
        let param_info = &PluginHandle::params(&self.plugin).params()[param_index];

        param_info.get_accessor().set(&self.plugin, value);

        let value_normalized = param_info.get_mapping().unmap(value);

        let _ = PluginHandle::params(&self.plugin)
            .index_of(id)
            .expect("Invalid parameter id");

        let component_handler = self.component_handler.borrow().clone();
        if let Some(component_handler) = &component_handler {
            unsafe {
                component_handler.performEdit(id, value_normalized);
            }
        }

        self.param_states
            .dirty_processor
            .set(param_index, Ordering::Release);
    }

    fn end_edit(&self, id: ParamId) {
        let _ = PluginHandle::params(&self.plugin)
            .index_of(id)
            .expect("Invalid parameter id");

        let component_handler = self.component_handler.borrow().clone();
        if let Some(component_handler) = &component_handler {
            unsafe {
                component_handler.endEdit(id);
            }
        }
    }

    fn poll_params(&self) -> PollParams<P> {
        PollParams {
            iter: self
                .param_states
                .dirty_editor
                .drain_indices(Ordering::Acquire),
            param_list: PluginHandle::params(&self.plugin),
        }
    }
}

struct BusStates {
    inputs: Vec<BusState>,
    outputs: Vec<BusState>,
}

struct ParamStates {
    dirty_processor: AtomicBitset,
    dirty_editor: AtomicBitset,
}

struct ProcessorState<P: Plugin> {
    sample_rate: f64,
    max_buffer_size: usize,
    needs_reset: bool,
    input_channels: usize,
    input_indices: Vec<(usize, usize)>,
    input_ptrs: Vec<*const f32>,
    output_channels: usize,
    output_indices: Vec<(usize, usize)>,
    output_ptrs: Vec<*mut f32>,
    // Scratch buffers for copying inputs to when the host uses the same
    // buffers for inputs and outputs
    scratch_buffers: Vec<f32>,
    output_ptr_set: Vec<*mut f32>,
    aliased_inputs: Vec<usize>,
    events: Vec<Event>,
    processor: Option<P::Processor>,
}

struct EditorState<P: Plugin> {
    context: Rc<Vst3EditorContext<P>>,
    editor: RefCell<Option<P::Editor>>,
}

struct Wrapper<P: Plugin> {
    has_editor: bool,
    bus_list: BusList,
    bus_config_set: HashSet<BusConfig>,
    // We only form an &mut to bus_states in set_bus_arrangements and
    // activate_bus, which aren't called concurrently with any other methods on
    // IComponent or IAudioProcessor per the spec.
    bus_states: UnsafeCell<BusStates>,
    param_states: Arc<ParamStates>,
    plugin: PluginHandle<P>,
    processor_state: UnsafeCell<ProcessorState<P>>,
    editor_state: UnsafeCell<Rc<EditorState<P>>>,
}

impl<P: Plugin> Wrapper<P> {
    pub fn new(info: &PluginInfo) -> Wrapper<P> {
        let bus_list = P::buses();
        let bus_config_list = P::bus_configs();

        util::validate_bus_configs(&bus_list, &bus_config_list);

        let bus_config_set = bus_config_list
            .get_configs()
            .iter()
            .cloned()
            .collect::<HashSet<BusConfig>>();

        let default_config = bus_config_list.get_default().unwrap();

        let mut inputs = Vec::with_capacity(bus_list.get_inputs().len());
        for format in default_config.get_inputs() {
            inputs.push(BusState::new(format.clone(), true));
        }

        let mut outputs = Vec::with_capacity(bus_list.get_outputs().len());
        for format in default_config.get_outputs() {
            outputs.push(BusState::new(format.clone(), true));
        }

        let bus_states = UnsafeCell::new(BusStates { inputs, outputs });

        let plugin = PluginHandle::<P>::new();

        let param_count = PluginHandle::params(&plugin).params().len();

        let dirty_processor = AtomicBitset::with_len(param_count);
        let dirty_editor = AtomicBitset::with_len(param_count);
        let param_states = Arc::new(ParamStates {
            dirty_processor,
            dirty_editor,
        });

        let input_indices = Vec::with_capacity(bus_list.get_inputs().len());
        let input_ptrs = Vec::new();

        let output_indices = Vec::with_capacity(bus_list.get_outputs().len());
        let output_ptrs = Vec::new();

        let processor_state = UnsafeCell::new(ProcessorState {
            sample_rate: 0.0,
            max_buffer_size: 0,
            needs_reset: false,
            input_channels: 0,
            input_indices,
            input_ptrs,
            output_channels: 0,
            output_indices,
            output_ptrs,
            scratch_buffers: Vec::new(),
            output_ptr_set: Vec::new(),
            aliased_inputs: Vec::new(),
            // We can't know the maximum number of param changes in a
            // block, so make a reasonable guess and hope we don't have to
            // allocate more
            events: Vec::with_capacity(1024 + 4 * param_count),
            processor: None,
        });

        let editor_context = Rc::new(Vst3EditorContext {
            component_handler: RefCell::new(None),
            plug_frame: RefCell::new(None),
            plugin: plugin.clone(),
            param_states: param_states.clone(),
        });

        let editor_state = UnsafeCell::new(Rc::new(EditorState {
            context: editor_context,
            editor: RefCell::new(None),
        }));

        Wrapper {
            has_editor: info.get_has_editor(),
            bus_list,
            bus_config_set,
            bus_states,
            param_states,
            plugin,
            processor_state,
            editor_state,
        }
    }
}

impl<P: Plugin> Class for Wrapper<P> {
    type Interfaces = (
        IPluginBase,
        IComponent,
        IAudioProcessor,
        IProcessContextRequirements,
        IEditController,
    );
}

impl<P: Plugin> IPluginBaseTrait for Wrapper<P> {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl<P: Plugin> IComponentTrait for Wrapper<P> {
    unsafe fn getControllerClassId(&self, _classId: *mut TUID) -> tresult {
        kNotImplemented
    }

    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }

    unsafe fn getBusCount(&self, type_: MediaType, dir: BusDirection) -> int32 {
        match type_ as MediaTypes {
            MediaTypes_::kAudio => match dir as BusDirections {
                BusDirections_::kInput => self.bus_list.get_inputs().len() as int32,
                BusDirections_::kOutput => self.bus_list.get_outputs().len() as int32,
                _ => 0,
            },
            MediaTypes_::kEvent => 0,
            _ => 0,
        }
    }

    unsafe fn getBusInfo(
        &self,
        type_: MediaType,
        dir: BusDirection,
        index: int32,
        bus: *mut BusInfo,
    ) -> tresult {
        let bus_states = &*self.bus_states.get();

        match type_ as MediaTypes {
            MediaTypes_::kAudio => {
                let bus_info = match dir as BusDirections {
                    BusDirections_::kInput => self.bus_list.get_inputs().get(index as usize),
                    BusDirections_::kOutput => self.bus_list.get_outputs().get(index as usize),
                    _ => None,
                };

                let bus_state = match dir as BusDirections {
                    BusDirections_::kInput => bus_states.inputs.get(index as usize),
                    BusDirections_::kOutput => bus_states.outputs.get(index as usize),
                    _ => None,
                };

                if let (Some(bus_info), Some(bus_state)) = (bus_info, bus_state) {
                    let bus = &mut *bus;

                    bus.mediaType = MediaTypes_::kAudio as MediaType;
                    bus.direction = dir;
                    bus.channelCount = bus_state.format().channels() as int32;
                    copy_wstring(bus_info.get_name(), &mut bus.name);
                    bus.busType = if index == 0 {
                        BusTypes_::kMain as BusType
                    } else {
                        BusTypes_::kAux as BusType
                    };
                    bus.flags = BusInfo_::BusFlags_::kDefaultActive as uint32;

                    return kResultOk;
                }
            }
            MediaTypes_::kEvent => {}
            _ => {}
        }

        kInvalidArgument
    }

    unsafe fn getRoutingInfo(
        &self,
        _inInfo: *mut RoutingInfo,
        _outInfo: *mut RoutingInfo,
    ) -> tresult {
        kNotImplemented
    }

    unsafe fn activateBus(
        &self,
        type_: MediaType,
        dir: BusDirection,
        index: int32,
        state: TBool,
    ) -> tresult {
        let bus_states = &mut *self.bus_states.get();

        match type_ as MediaTypes {
            MediaTypes_::kAudio => {
                let bus_state = match dir as BusDirections {
                    BusDirections_::kInput => bus_states.inputs.get_mut(index as usize),
                    BusDirections_::kOutput => bus_states.outputs.get_mut(index as usize),
                    _ => None,
                };

                if let Some(bus_state) = bus_state {
                    bus_state.set_enabled(if state == 0 { false } else { true });
                    return kResultOk;
                }
            }
            MediaTypes_::kEvent => {}
            _ => {}
        }

        kInvalidArgument
    }

    unsafe fn setActive(&self, state: TBool) -> tresult {
        let bus_states = &mut *self.bus_states.get();
        let processor_state = &mut *self.processor_state.get();

        match state {
            0 => {
                processor_state.processor = None;
            }
            _ => {
                let context = ProcessContext::new(
                    processor_state.sample_rate,
                    processor_state.max_buffer_size,
                    &bus_states.inputs[..],
                    &bus_states.outputs[..],
                );
                processor_state.processor =
                    Some(P::Processor::create(self.plugin.clone(), &context));

                // Prepare buffer indices and ensure that buffer pointer Vecs are the correct size:

                processor_state.input_indices.clear();
                let mut total_channels = 0;
                for bus_state in bus_states.inputs.iter() {
                    let channels = if bus_state.enabled() {
                        bus_state.format().channels()
                    } else {
                        0
                    };
                    processor_state
                        .input_indices
                        .push((total_channels, total_channels + channels));
                    total_channels += channels;
                }
                processor_state.input_channels = total_channels;

                processor_state
                    .input_ptrs
                    .reserve(processor_state.input_channels);
                processor_state
                    .input_ptrs
                    .shrink_to(processor_state.input_channels);

                processor_state.output_indices.clear();
                let mut total_channels = 0;
                for bus_state in bus_states.outputs.iter() {
                    let channels = if bus_state.enabled() {
                        bus_state.format().channels()
                    } else {
                        0
                    };
                    processor_state
                        .output_indices
                        .push((total_channels, total_channels + channels));
                    total_channels += channels;
                }
                processor_state.output_channels = total_channels;

                processor_state
                    .output_ptrs
                    .reserve(processor_state.output_channels);
                processor_state
                    .output_ptrs
                    .shrink_to(processor_state.output_channels);

                // Ensure enough scratch buffer space for any number of aliased input buffers:

                let scratch_buffer_size = processor_state.max_buffer_size
                    * processor_state
                        .input_channels
                        .min(processor_state.output_channels);
                processor_state.scratch_buffers.reserve(scratch_buffer_size);
                processor_state
                    .scratch_buffers
                    .shrink_to(scratch_buffer_size);

                processor_state
                    .output_ptr_set
                    .reserve(processor_state.output_channels);
                processor_state
                    .output_ptr_set
                    .shrink_to(processor_state.output_channels);

                processor_state
                    .aliased_inputs
                    .reserve(processor_state.input_channels);
                processor_state
                    .aliased_inputs
                    .shrink_to(processor_state.input_channels);
            }
        }

        kResultOk
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        struct StreamReader<'a>(ComRef<'a, IBStream>);

        impl<'a> io::Read for StreamReader<'a> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let mut bytes: int32 = 0;
                let result = unsafe {
                    self.0.read(
                        buf.as_mut_ptr() as *mut c_void,
                        buf.len() as int32,
                        &mut bytes,
                    )
                };

                if result == kResultOk {
                    Ok(bytes as usize)
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::Other,
                        "Failed to read from stream",
                    ))
                }
            }
        }

        if let Some(state) = ComRef::from_raw(state) {
            if let Ok(_) = self.plugin.deserialize(&mut StreamReader(state)) {
                self.param_states.dirty_processor.set_all(Ordering::Release);
                self.param_states.dirty_editor.set_all(Ordering::Release);

                return kResultOk;
            }
        }

        kResultFalse
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        struct StreamWriter<'a>(ComRef<'a, IBStream>);

        impl<'a> io::Write for StreamWriter<'a> {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                let mut bytes: int32 = 0;
                let result = unsafe {
                    self.0
                        .write(buf.as_ptr() as *mut c_void, buf.len() as int32, &mut bytes)
                };

                if result == kResultOk {
                    Ok(bytes as usize)
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::Other,
                        "Failed to write to stream",
                    ))
                }
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        if let Some(state) = ComRef::from_raw(state) {
            if let Ok(_) = self.plugin.serialize(&mut StreamWriter(state)) {
                self.param_states.dirty_processor.set_all(Ordering::Release);
                self.param_states.dirty_editor.set_all(Ordering::Release);

                return kResultOk;
            }
        }

        kResultFalse
    }
}

impl<P: Plugin> IAudioProcessorTrait for Wrapper<P> {
    unsafe fn setBusArrangements(
        &self,
        inputs: *mut SpeakerArrangement,
        numIns: int32,
        outputs: *mut SpeakerArrangement,
        numOuts: int32,
    ) -> tresult {
        let bus_states = &mut *self.bus_states.get();

        if numIns as usize != self.bus_list.get_inputs().len()
            || numOuts as usize != self.bus_list.get_outputs().len()
        {
            return kResultFalse;
        }

        let mut candidate = BusConfig::new();

        // Don't use from_raw_parts for zero-length inputs, since the pointer
        // may be null or unaligned
        let inputs = if numIns > 0 {
            slice::from_raw_parts(inputs, numIns as usize)
        } else {
            &[]
        };
        for input in inputs {
            if let Some(bus_format) = speaker_arrangement_to_bus_format(*input) {
                candidate = candidate.input(bus_format);
            } else {
                return kResultFalse;
            }
        }

        // Don't use from_raw_parts for zero-length inputs, since the pointer
        // may be null or unaligned
        let outputs = if numOuts > 0 {
            slice::from_raw_parts(outputs, numOuts as usize)
        } else {
            &[]
        };
        for output in outputs {
            if let Some(bus_format) = speaker_arrangement_to_bus_format(*output) {
                candidate = candidate.output(bus_format);
            } else {
                return kResultFalse;
            }
        }

        if self.bus_config_set.contains(&candidate) {
            for (input, bus_state) in candidate
                .get_inputs()
                .iter()
                .zip(bus_states.inputs.iter_mut())
            {
                bus_state.set_format(input.clone());
            }

            for (output, bus_state) in candidate
                .get_outputs()
                .iter()
                .zip(bus_states.outputs.iter_mut())
            {
                bus_state.set_format(output.clone());
            }

            return kResultTrue;
        }

        kResultFalse
    }

    unsafe fn getBusArrangement(
        &self,
        dir: BusDirection,
        index: int32,
        arr: *mut SpeakerArrangement,
    ) -> tresult {
        let bus_states = &*self.bus_states.get();

        let bus_state = match dir as BusDirections {
            BusDirections_::kInput => bus_states.inputs.get(index as usize),
            BusDirections_::kOutput => bus_states.outputs.get(index as usize),
            _ => None,
        };

        if let Some(bus_state) = bus_state {
            *arr = bus_format_to_speaker_arrangement(bus_state.format());
            return kResultOk;
        }

        kInvalidArgument
    }

    unsafe fn canProcessSampleSize(&self, symbolicSampleSize: int32) -> tresult {
        match symbolicSampleSize as SymbolicSampleSizes {
            SymbolicSampleSizes_::kSample32 => kResultTrue,
            SymbolicSampleSizes_::kSample64 => kResultFalse,
            _ => kInvalidArgument,
        }
    }

    unsafe fn getLatencySamples(&self) -> uint32 {
        0
    }

    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        let processor_state = &mut *self.processor_state.get();

        let setup = &*setup;

        processor_state.sample_rate = setup.sampleRate;
        processor_state.max_buffer_size = setup.maxSamplesPerBlock as usize;

        kResultOk
    }

    unsafe fn setProcessing(&self, state: TBool) -> tresult {
        let bus_states = &*self.bus_states.get();
        let processor_state = &mut *self.processor_state.get();

        if processor_state.processor.is_none() {
            return kNotInitialized;
        }

        if state != 0 {
            // Don't need to call reset() the first time set_processing() is
            // called with true.
            if !processor_state.needs_reset {
                processor_state.needs_reset = true;
                return kResultOk;
            }

            let context = ProcessContext::new(
                processor_state.sample_rate,
                processor_state.max_buffer_size,
                &bus_states.inputs[..],
                &bus_states.outputs[..],
            );
            processor_state.processor.as_mut().unwrap().reset(&context);
        }

        kResultOk
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        let bus_states = &*self.bus_states.get();
        let processor_state = &mut *self.processor_state.get();

        if processor_state.processor.is_none() {
            return kNotInitialized;
        }

        processor_state.events.clear();

        for index in self
            .param_states
            .dirty_processor
            .drain_indices(Ordering::Acquire)
        {
            let param_info = &PluginHandle::params(&self.plugin).params()[index];
            let value = param_info.get_accessor().get(&self.plugin);

            processor_state.events.push(Event {
                offset: 0,
                event: EventType::ParamChange(ParamChange {
                    id: param_info.get_id(),
                    value,
                }),
            });
        }

        let process_data = &*data;

        if let Some(param_changes) = ComRef::from_raw(process_data.inputParameterChanges) {
            for index in 0..param_changes.getParameterCount() {
                let param_data = param_changes.getParameterData(index);

                let Some(param_data) = ComRef::from_raw(param_data) else { continue; };

                let id = param_data.getParameterId();
                let point_count = param_data.getPointCount();

                if let Some(param_index) = PluginHandle::params(&self.plugin).index_of(id) {
                    for index in 0..point_count {
                        let mut offset = 0;
                        let mut value_normalized = 0.0;
                        let result = param_data.getPoint(index, &mut offset, &mut value_normalized);

                        if result != kResultOk {
                            continue;
                        }

                        let param_info = &PluginHandle::params(&self.plugin).params()[param_index];
                        let value = param_info.get_mapping().map(value_normalized);
                        param_info.get_accessor().set(&self.plugin, value);
                        self.param_states
                            .dirty_editor
                            .set(param_index, Ordering::Release);

                        processor_state.events.push(Event {
                            offset: offset as usize,
                            event: EventType::ParamChange(ParamChange { id, value }),
                        });
                    }
                }
            }
        }

        processor_state
            .events
            .sort_by_key(|param_change| param_change.offset);

        processor_state.input_ptrs.clear();
        processor_state.output_ptrs.clear();

        let samples = process_data.numSamples as usize;

        if samples > 0 {
            if self.bus_list.get_inputs().len() > 0 {
                if process_data.numInputs as usize != self.bus_list.get_inputs().len() {
                    return kInvalidArgument;
                }

                let inputs =
                    slice::from_raw_parts(process_data.inputs, process_data.numInputs as usize);

                for (input, bus_state) in inputs.iter().zip(bus_states.inputs.iter()) {
                    if !bus_state.enabled() || bus_state.format().channels() == 0 {
                        continue;
                    }

                    if input.numChannels as usize != bus_state.format().channels() {
                        return kInvalidArgument;
                    }

                    let channels = slice::from_raw_parts(
                        input.__field0.channelBuffers32 as *const *const f32,
                        input.numChannels as usize,
                    );
                    processor_state.input_ptrs.extend_from_slice(channels);
                }
            }

            if self.bus_list.get_outputs().len() > 0 {
                if process_data.numOutputs as usize != self.bus_list.get_outputs().len() {
                    return kInvalidArgument;
                }

                let outputs =
                    slice::from_raw_parts(process_data.outputs, process_data.numOutputs as usize);

                for (output, bus_state) in outputs.iter().zip(bus_states.outputs.iter()) {
                    if !bus_state.enabled() || bus_state.format().channels() == 0 {
                        continue;
                    }

                    if output.numChannels as usize != bus_state.format().channels() {
                        return kInvalidArgument;
                    }

                    let channels = slice::from_raw_parts(
                        output.__field0.channelBuffers32 as *const *mut f32,
                        output.numChannels as usize,
                    );
                    processor_state.output_ptrs.extend_from_slice(channels);
                }
            }

            // Copy aliased input buffers into scratch buffers

            processor_state
                .output_ptr_set
                .extend_from_slice(&processor_state.output_ptrs);
            processor_state.output_ptr_set.sort();
            processor_state.output_ptr_set.dedup();

            for (channel, input_ptr) in processor_state.input_ptrs.iter().enumerate() {
                if processor_state
                    .output_ptr_set
                    .binary_search(&(*input_ptr as *mut f32))
                    .is_ok()
                {
                    processor_state.aliased_inputs.push(channel);

                    let input_buffer = slice::from_raw_parts(*input_ptr, samples);
                    processor_state
                        .scratch_buffers
                        .extend_from_slice(input_buffer);
                }
            }

            for (index, channel) in processor_state.aliased_inputs.iter().enumerate() {
                let offset = index * processor_state.max_buffer_size;
                let ptr = processor_state.scratch_buffers.as_ptr().add(offset) as *mut f32;
                processor_state.input_ptrs[*channel] = ptr;
            }

            processor_state.output_ptr_set.clear();
            processor_state.aliased_inputs.clear();
        } else {
            processor_state
                .input_ptrs
                .resize(processor_state.input_channels, ptr::null());
            processor_state
                .output_ptrs
                .resize(processor_state.output_channels, ptr::null_mut());
        }

        let buffers = Buffers::new(
            samples,
            &bus_states.inputs,
            &processor_state.input_indices,
            &processor_state.input_ptrs,
            &bus_states.outputs,
            &processor_state.output_indices,
            &processor_state.output_ptrs,
        );

        let context = ProcessContext::new(
            processor_state.sample_rate,
            processor_state.max_buffer_size,
            &bus_states.inputs[..],
            &bus_states.outputs[..],
        );

        if let Some(processor) = &mut processor_state.processor {
            processor.process(&context, buffers, &processor_state.events[..]);
        }

        processor_state.scratch_buffers.clear();

        processor_state.input_ptrs.clear();
        processor_state.output_ptrs.clear();

        processor_state.events.clear();

        kResultOk
    }

    unsafe fn getTailSamples(&self) -> uint32 {
        kInfiniteTail
    }
}

impl<P: Plugin> IProcessContextRequirementsTrait for Wrapper<P> {
    unsafe fn getProcessContextRequirements(&self) -> uint32 {
        0
    }
}

impl<P: Plugin> IEditControllerTrait for Wrapper<P> {
    unsafe fn setComponentState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getParameterCount(&self) -> int32 {
        PluginHandle::params(&self.plugin).params().len() as int32
    }

    unsafe fn getParameterInfo(&self, paramIndex: int32, info: *mut ParameterInfo) -> tresult {
        let params = PluginHandle::params(&self.plugin);
        if let Some(param_info) = params.params().get(paramIndex as usize) {
            let info = &mut *info;

            info.id = param_info.get_id();
            copy_wstring(&param_info.get_name(), &mut info.title);
            copy_wstring(&param_info.get_name(), &mut info.shortTitle);
            copy_wstring(&param_info.get_label(), &mut info.units);
            info.stepCount = if let Some(steps) = param_info.get_steps() {
                (steps.max(2) - 1) as i32
            } else {
                0
            };
            info.defaultNormalizedValue = param_info.get_mapping().unmap(param_info.get_default());
            info.unitId = 0;
            info.flags = ParameterInfo_::ParameterFlags_::kCanAutomate as int32;

            kResultOk
        } else {
            kInvalidArgument
        }
    }

    unsafe fn getParamStringByValue(
        &self,
        id: ParamID,
        valueNormalized: ParamValue,
        string: *mut String128,
    ) -> tresult {
        if let Some(param_info) = PluginHandle::params(&self.plugin).get(id) {
            let mut display = String::new();
            let value = param_info.get_mapping().map(valueNormalized);
            param_info.get_format().display(value, &mut display);
            copy_wstring(&display, &mut *string);

            return kResultOk;
        }

        kInvalidArgument
    }

    unsafe fn getParamValueByString(
        &self,
        id: ParamID,
        string: *mut TChar,
        valueNormalized: *mut ParamValue,
    ) -> tresult {
        if let Some(param_info) = PluginHandle::params(&self.plugin).get(id) {
            let len = len_wstring(string);
            if let Ok(string) = String::from_utf16(slice::from_raw_parts(string as *const u16, len))
            {
                if let Ok(value) = param_info.get_format().parse(&string) {
                    *valueNormalized = param_info.get_mapping().unmap(value);
                    return kResultOk;
                }
            }
        }

        kInvalidArgument
    }

    unsafe fn normalizedParamToPlain(
        &self,
        id: ParamID,
        valueNormalized: ParamValue,
    ) -> ParamValue {
        if let Some(param_info) = PluginHandle::params(&self.plugin).get(id) {
            return param_info.get_mapping().map(valueNormalized);
        }

        0.0
    }

    unsafe fn plainParamToNormalized(&self, id: ParamID, plainValue: ParamValue) -> ParamValue {
        if let Some(param_info) = PluginHandle::params(&self.plugin).get(id) {
            return param_info.get_mapping().unmap(plainValue);
        }

        0.0
    }

    unsafe fn getParamNormalized(&self, id: ParamID) -> ParamValue {
        if let Some(param_info) = PluginHandle::params(&self.plugin).get(id) {
            let value = param_info.get_accessor().get(&self.plugin);
            return param_info.get_mapping().unmap(value);
        }

        0.0
    }

    unsafe fn setParamNormalized(&self, id: ParamID, value: ParamValue) -> tresult {
        if let Some(param_info) = PluginHandle::params(&self.plugin).get(id) {
            let param_index = PluginHandle::params(&self.plugin).index_of(id).unwrap();

            let value = param_info.get_mapping().map(value);
            param_info.get_accessor().set(&self.plugin, value);

            self.param_states
                .dirty_processor
                .set(param_index, Ordering::Release);
            self.param_states
                .dirty_editor
                .set(param_index, Ordering::Release);

            return kResultOk;
        }

        kInvalidArgument
    }

    unsafe fn setComponentHandler(&self, handler: *mut IComponentHandler) -> tresult {
        let editor_state = &*self.editor_state.get();

        if let Some(handler) = ComRef::from_raw(handler) {
            editor_state
                .context
                .component_handler
                .replace(Some(handler.to_com_ptr()));
        }

        kResultOk
    }

    unsafe fn createView(&self, name: FIDString) -> *mut IPlugView {
        if !self.has_editor {
            return ptr::null_mut();
        }

        let editor_state = &*self.editor_state.get();

        if CStr::from_ptr(name) == CStr::from_ptr(ViewType::kEditor) {
            let view = ComWrapper::new(View::<P>::new(&editor_state));
            return view.to_com_ptr::<IPlugView>().unwrap().into_raw();
        }

        ptr::null_mut()
    }
}

struct View<P: Plugin> {
    state: Rc<EditorState<P>>,
    #[cfg(target_os = "linux")]
    handler: ComWrapper<linux::EventHandler<P>>,
}

impl<P: Plugin> View<P> {
    pub fn new(state: &Rc<EditorState<P>>) -> View<P> {
        View {
            state: state.clone(),
            #[cfg(target_os = "linux")]
            handler: ComWrapper::new(linux::EventHandler::new(state)),
        }
    }
}

impl<P: Plugin> Class for View<P> {
    type Interfaces = (IPlugView,);
}

impl<P: Plugin> IPlugViewTrait for View<P> {
    unsafe fn isPlatformTypeSupported(&self, type_: FIDString) -> tresult {
        #[cfg(target_os = "windows")]
        if CStr::from_ptr(type_) == CStr::from_ptr(kPlatformTypeHWND) {
            return kResultTrue;
        }

        #[cfg(target_os = "macos")]
        if CStr::from_ptr(type_) == CStr::from_ptr(kPlatformTypeNSView) {
            return kResultTrue;
        }

        #[cfg(target_os = "linux")]
        if CStr::from_ptr(type_) == CStr::from_ptr(kPlatformTypeX11EmbedWindowID) {
            return kResultTrue;
        }

        kResultFalse
    }

    unsafe fn attached(&self, parent: *mut c_void, type_: FIDString) -> tresult {
        if self.isPlatformTypeSupported(type_) != kResultTrue {
            return kNotImplemented;
        }

        #[cfg(target_os = "macos")]
        let parent = {
            use raw_window_handle::macos::MacOSHandle;
            RawWindowHandle::MacOS(MacOSHandle {
                ns_view: parent,
                ..MacOSHandle::empty()
            })
        };

        #[cfg(target_os = "windows")]
        let parent = {
            use raw_window_handle::windows::WindowsHandle;
            RawWindowHandle::Windows(WindowsHandle {
                hwnd: parent,
                ..WindowsHandle::empty()
            })
        };

        #[cfg(target_os = "linux")]
        let parent = {
            use raw_window_handle::unix::XcbHandle;
            RawWindowHandle::Xcb(XcbHandle {
                window: parent as u32,
                ..XcbHandle::empty()
            })
        };

        let context = EditorContext::new(self.state.context.clone());

        let editor = P::Editor::open(
            self.state.context.plugin.clone(),
            context,
            Some(&ParentWindow(parent)),
        );

        #[cfg(target_os = "linux")]
        {
            use vst3_bindgen::Steinberg::Linux::*;

            let Some(frame) = self.state.context.plug_frame.borrow().clone() else {
                return kNotInitialized;
            };

            if let Some(run_loop) = frame.cast::<IRunLoop>() {
                let timer_handler = self.handler.as_com_ref::<ITimerHandler>().unwrap();
                run_loop.registerTimer(timer_handler.as_ptr(), 16);

                if let Some(fd) = editor.file_descriptor() {
                    let event_handler = self.handler.as_com_ref::<IEventHandler>().unwrap();
                    run_loop.registerEventHandler(event_handler.as_ptr(), fd);
                }
            }
        }

        self.state.editor.replace(Some(editor));

        kResultOk
    }

    unsafe fn removed(&self) -> tresult {
        if let Some(mut editor) = self.state.editor.take() {
            editor.close();
        }

        #[cfg(target_os = "linux")]
        {
            use vst3_bindgen::Steinberg::Linux::*;

            let Some(frame) = self.state.context.plug_frame.borrow().clone() else {
                return kNotInitialized;
            };

            if let Some(run_loop) = frame.cast::<IRunLoop>() {
                let timer_handler = self.handler.as_com_ref::<ITimerHandler>().unwrap();
                run_loop.unregisterTimer(timer_handler.as_ptr());

                let event_handler = self.handler.as_com_ref::<IEventHandler>().unwrap();
                run_loop.unregisterEventHandler(event_handler.as_ptr());
            }
        }

        kResultOk
    }

    unsafe fn onWheel(&self, _distance: f32) -> tresult {
        kNotImplemented
    }

    unsafe fn onKeyDown(&self, _key: char16, _keyCode: int16, _modifiers: int16) -> tresult {
        kNotImplemented
    }

    unsafe fn onKeyUp(&self, _key: char16, _keyCode: int16, _modifiers: int16) -> tresult {
        kNotImplemented
    }

    unsafe fn getSize(&self, size: *mut ViewRect) -> tresult {
        let (width, height) = P::Editor::size();

        let size = &mut *size;
        size.top = 0;
        size.left = 0;
        size.right = width.round() as int32;
        size.bottom = height.round() as int32;

        kResultOk
    }

    unsafe fn onSize(&self, _newSize: *mut ViewRect) -> tresult {
        kNotImplemented
    }

    unsafe fn onFocus(&self, _state: TBool) -> tresult {
        kNotImplemented
    }

    unsafe fn setFrame(&self, frame: *mut IPlugFrame) -> tresult {
        if let Some(frame) = ComRef::from_raw(frame) {
            self.state
                .context
                .plug_frame
                .replace(Some(frame.to_com_ptr()));
        }

        kResultOk
    }

    unsafe fn canResize(&self) -> tresult {
        kResultFalse
    }

    unsafe fn checkSizeConstraint(&self, _rect: *mut ViewRect) -> tresult {
        kNotImplemented
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use vst3_bindgen::Steinberg::Linux::*;

    pub(super) struct EventHandler<P: Plugin> {
        state: Rc<EditorState<P>>,
    }

    impl<P: Plugin> EventHandler<P> {
        pub fn new(state: &Rc<EditorState<P>>) -> EventHandler<P> {
            EventHandler {
                state: state.clone(),
            }
        }
    }

    impl<P: Plugin> Class for EventHandler<P> {
        type Interfaces = (IEventHandler, ITimerHandler);
    }

    impl<P: Plugin> IEventHandlerTrait for EventHandler<P> {
        unsafe fn onFDIsSet(&self, _fd: FileDescriptor) {
            if let Ok(mut editor) = self.state.editor.try_borrow_mut() {
                if let Some(editor) = &mut *editor {
                    editor.poll();
                }
            }
        }
    }

    impl<P: Plugin> ITimerHandlerTrait for EventHandler<P> {
        unsafe fn onTimer(&self) {
            if let Ok(mut editor) = self.state.editor.try_borrow_mut() {
                if let Some(editor) = &mut *editor {
                    editor.poll();
                }
            }
        }
    }
}

struct Factory<P> {
    vst3_info: Vst3Info,
    info: PluginInfo,
    _marker: PhantomData<P>,
}

impl<P: Plugin + Vst3Plugin> Factory<P> {
    pub fn new() -> Factory<P> {
        Factory {
            vst3_info: P::vst3_info(),
            info: P::info(),
            _marker: PhantomData,
        }
    }
}

impl<P: Plugin + Vst3Plugin> Class for Factory<P> {
    type Interfaces = (IPluginFactory3,);
}

impl<P: Plugin + Vst3Plugin> IPluginFactoryTrait for Factory<P> {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        let info = &mut *info;

        copy_cstring(&self.info.get_vendor(), &mut info.vendor);
        copy_cstring(&self.info.get_url(), &mut info.url);
        copy_cstring(&self.info.get_email(), &mut info.email);
        info.flags = PFactoryInfo_::FactoryFlags_::kUnicode as int32;

        kResultOk
    }

    unsafe fn countClasses(&self) -> int32 {
        1
    }

    unsafe fn getClassInfo(&self, index: int32, info: *mut PClassInfo) -> tresult {
        if index != 0 {
            return kInvalidArgument;
        }

        let info = &mut *info;

        info.cid = self.vst3_info.get_class_id().0;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
        copy_cstring("Audio Module Class", &mut info.category);
        copy_cstring(&self.info.get_name(), &mut info.name);

        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: FIDString,
        iid: FIDString,
        obj: *mut *mut c_void,
    ) -> tresult {
        let cid = &*(cid as *const TUID);
        if cid != &self.vst3_info.get_class_id().0 {
            return kInvalidArgument;
        }

        let wrapper = ComWrapper::new(Wrapper::<P>::new(&self.info));
        let unknown = wrapper.to_com_ptr::<FUnknown>().unwrap();
        let ptr = unknown.as_ptr();
        ((*(*ptr).vtbl).queryInterface)(ptr, iid as *const TUID, obj)
    }
}

impl<P: Plugin + Vst3Plugin> IPluginFactory2Trait for Factory<P> {
    unsafe fn getClassInfo2(&self, index: int32, info: *mut PClassInfo2) -> tresult {
        if index != 0 {
            return kInvalidArgument;
        }

        let info = &mut *info;

        info.cid = self.vst3_info.get_class_id().0;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
        copy_cstring("Audio Module Class", &mut info.category);
        copy_cstring(&self.info.get_name(), &mut info.name);
        info.classFlags = 0;
        copy_cstring("Fx", &mut info.subCategories);
        copy_cstring(&self.info.get_vendor(), &mut info.vendor);
        copy_cstring("", &mut info.version);
        let version_str = CStr::from_ptr(SDKVersionString).to_str().unwrap();
        copy_cstring(version_str, &mut info.sdkVersion);

        kResultOk
    }
}

impl<P: Plugin + Vst3Plugin> IPluginFactory3Trait for Factory<P> {
    unsafe fn getClassInfoUnicode(&self, index: int32, info: *mut PClassInfoW) -> tresult {
        if index != 0 {
            return kInvalidArgument;
        }

        let info = &mut *info;

        info.cid = self.vst3_info.get_class_id().0;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
        copy_cstring("Audio Module Class", &mut info.category);
        copy_wstring(&self.info.get_name(), &mut info.name);
        info.classFlags = 0;
        copy_cstring("Fx", &mut info.subCategories);
        copy_wstring(&self.info.get_vendor(), &mut info.vendor);
        copy_wstring("", &mut info.version);
        let version_str = CStr::from_ptr(SDKVersionString).to_str().unwrap();
        copy_wstring(version_str, &mut info.sdkVersion);

        kResultOk
    }

    unsafe fn setHostContext(&self, _context: *mut FUnknown) -> tresult {
        kNotImplemented
    }
}

#[derive(Copy, Clone)]
pub struct Uid(TUID);

impl Uid {
    pub const fn new(a: u32, b: u32, c: u32, d: u32) -> Uid {
        Uid(uid(a, b, c, d))
    }
}

pub struct Vst3Info {
    class_id: Uid,
}

impl Vst3Info {
    #[inline]
    pub fn with_class_id(class_id: Uid) -> Vst3Info {
        Vst3Info { class_id }
    }

    #[inline]
    pub fn class_id(mut self, class_id: Uid) -> Self {
        self.class_id = class_id;
        self
    }

    #[inline]
    pub fn get_class_id(&self) -> Uid {
        self.class_id
    }
}

pub trait Vst3Plugin {
    fn vst3_info() -> Vst3Info;
}

#[doc(hidden)]
pub fn get_plugin_factory<P: Plugin + Vst3Plugin>() -> *mut c_void {
    ComWrapper::new(Factory::<P>::new())
        .to_com_ptr::<IPluginFactory>()
        .unwrap()
        .into_raw() as *mut c_void
}

#[macro_export]
macro_rules! vst3 {
    ($plugin:ty) => {
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
        extern "system" fn BundleEntry(_bundle_ref: *mut ::std::ffi::c_void) -> bool {
            true
        }

        #[cfg(target_os = "macos")]
        #[no_mangle]
        extern "system" fn BundleExit() -> bool {
            true
        }

        #[cfg(target_os = "linux")]
        #[no_mangle]
        extern "system" fn ModuleEntry(_library_handle: *mut ::std::ffi::c_void) -> bool {
            true
        }

        #[cfg(target_os = "linux")]
        #[no_mangle]
        extern "system" fn ModuleExit() -> bool {
            true
        }

        #[no_mangle]
        extern "system" fn GetPluginFactory() -> *mut ::std::ffi::c_void {
            ::coupler::format::vst3::get_plugin_factory::<$plugin>()
        }
    };
}
