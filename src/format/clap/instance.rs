use std::collections::HashMap;
use std::iter::zip;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::slice;
use std::sync::Arc;

use clack_extensions::params::implementation::{
    ParamDisplayWriter, ParamInfoWriter, PluginAudioProcessorParams, PluginMainThreadParams,
};
use clack_extensions::params::info::{ParamInfoData, ParamInfoFlags};
use clack_extensions::{
    audio_ports::*, audio_ports_config::*, gui::PluginGui, params::*, state::*,
};
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::events::Event as ClackEvent;
use clack_plugin::prelude::Plugin as ClackPlugin;
use clack_plugin::prelude::*;
use clack_plugin::stream::{InputStream, OutputStream};
use clack_plugin::utils::Cookie;

use crate::buffers::{BufferData, BufferType, BufferView, Buffers, RawBuffers};
use crate::bus::{BusDir, Format};
use crate::events::{Data, Event, Events};
use crate::format::clap::ClapPlugin;
use crate::params::{ParamId, ParamInfo, ParamValue};
use crate::plugin::{Host, Plugin, PluginInfo};
use crate::process::{Config, Processor};
use crate::sync::params::ParamValues;
use crate::util::DisplayParam;

fn port_type_from_format(format: &Format) -> AudioPortType<'static> {
    match format {
        Format::Mono => AudioPortType::MONO,
        Format::Stereo => AudioPortType::STEREO,
    }
}

fn map_param_in(param: &ParamInfo, value: f64) -> ParamValue {
    if let Some(steps) = param.steps {
        (value + 0.5) / steps as f64
    } else {
        value
    }
}

fn map_param_out(param: &ParamInfo, value: ParamValue) -> f64 {
    if let Some(steps) = param.steps {
        (value * steps as f64).floor()
    } else {
        value
    }
}

pub struct MainThreadState<'a, P: Plugin> {
    pub layout_index: usize,
    pub plugin: P,
    pub editor: Option<P::Editor>,
    pub instance: &'a InstanceShared<P>,
}

pub struct ProcessState<'a, P: Plugin> {
    buffer_data: Vec<BufferData>,
    buffer_ptrs: Vec<*mut f32>,
    events: Vec<Event>,
    processor: P::Processor,
    instance: &'a InstanceShared<P>,
}

// Due to buffer_ptrs
unsafe impl<'a, P: Plugin> Send for ProcessState<'a, P> {}

pub struct Instance<P: Plugin>(PhantomData<fn() -> P>);

impl<P: Plugin + ClapPlugin> ClackPlugin for Instance<P> {
    type AudioProcessor<'a> = ProcessState<'a, P>;
    type Shared<'a> = InstanceShared<P>;
    type MainThread<'a> = MainThreadState<'a, P>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, shared: &Self::Shared<'_>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginAudioPortsConfig>()
            .register::<PluginParams>()
            .register::<PluginState>();

        if shared.info.has_editor {
            builder.register::<PluginGui>();
        }
    }
}

pub struct InstanceShared<P: Plugin> {
    pub info: Arc<PluginInfo>,
    pub input_bus_map: Vec<usize>,
    pub output_bus_map: Vec<usize>,
    pub param_map: HashMap<ParamId, usize>,
    pub plugin_params: ParamValues,
    pub processor_params: ParamValues,
    _plugin: PhantomData<fn() -> P>,
}

impl<'a, P: Plugin> PluginShared<'a> for InstanceShared<P> {}

impl<'a, P: Plugin> InstanceShared<P> {
    pub fn new(_host: HostHandle<'a>, info: &Arc<PluginInfo>) -> Result<Self, PluginError> {
        let mut input_bus_map = Vec::new();
        let mut output_bus_map = Vec::new();
        for (index, bus) in info.buses.iter().enumerate() {
            match bus.dir {
                BusDir::In => input_bus_map.push(index),
                BusDir::Out => output_bus_map.push(index),
                BusDir::InOut => {
                    input_bus_map.push(index);
                    output_bus_map.push(index);
                }
            }
        }

        let mut param_map = HashMap::new();
        for (index, param) in info.params.iter().enumerate() {
            param_map.insert(param.id, index);
        }

        Ok(Self {
            plugin_params: ParamValues::new(&info.params),
            processor_params: ParamValues::new(&info.params),
            info: info.clone(),
            input_bus_map,
            output_bus_map,
            param_map,
            _plugin: PhantomData,
        })
    }

    fn sync_plugin(&self, plugin: &mut P) {
        for (index, value) in self.plugin_params.poll() {
            let id = self.info.params[index].id;
            plugin.set_param(id, value);
        }
    }

    fn sync_processor(&self, processor: &mut P::Processor) {
        for (index, value) in self.processor_params.poll() {
            let id = self.info.params[index].id;
            processor.set_param(id, value);
        }
    }
}

impl<'a, P: Plugin> PluginMainThread<'a, InstanceShared<P>> for MainThreadState<'a, P> {}
impl<'a, P: Plugin> MainThreadState<'a, P> {
    pub fn new(
        _host: HostMainThreadHandle<'a>,
        instance: &'a InstanceShared<P>,
    ) -> Result<Self, PluginError> {
        Ok(Self {
            layout_index: 0,
            plugin: P::new(Host {}),
            editor: None,
            instance,
        })
    }
}

impl<'a, P: Plugin> PluginAudioProcessor<'a, InstanceShared<P>, MainThreadState<'a, P>>
    for ProcessState<'a, P>
{
    fn activate(
        _host: HostAudioThreadHandle<'a>,
        main_thread_state: &mut MainThreadState<P>,
        instance: &'a InstanceShared<P>,
        audio_config: AudioConfiguration,
    ) -> Result<Self, PluginError> {
        let layout = &instance.info.layouts[main_thread_state.layout_index];

        let mut buffer_data = Vec::new();
        let mut total_channels = 0;
        for (info, format) in zip(&instance.info.buses, &layout.formats) {
            let buffer_type = match info.dir {
                BusDir::In => BufferType::Const,
                BusDir::Out | BusDir::InOut => BufferType::Mut,
            };
            let channel_count = format.channel_count();

            buffer_data.push(BufferData {
                buffer_type,
                start: total_channels,
                end: total_channels + channel_count,
            });

            total_channels += channel_count;
        }

        let config = Config {
            layout: layout.clone(),
            sample_rate: audio_config.sample_rate,
            max_buffer_size: audio_config.max_sample_count as usize,
        };

        instance.sync_plugin(&mut main_thread_state.plugin);

        Ok(ProcessState {
            buffer_data,
            buffer_ptrs: vec![NonNull::dangling().as_ptr(); total_channels],
            events: Vec::with_capacity(4096),
            processor: main_thread_state.plugin.processor(config),
            instance,
        })
    }

    fn reset(&mut self) {
        self.instance.sync_processor(&mut self.processor);
        self.processor.reset();
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: clack_plugin::prelude::Events,
    ) -> Result<ProcessStatus, PluginError> {
        let len = audio.frames_count() as usize;

        let input_count = audio.input_port_count();
        let output_count = audio.output_port_count();
        if input_count != self.instance.input_bus_map.len()
            || output_count != self.instance.output_bus_map.len()
        {
            return Err(PluginError::Message("Input/Output ports mismatch"));
        }

        for (&bus_index, mut output) in zip(&self.instance.output_bus_map, audio.output_ports()) {
            let data = &self.buffer_data[bus_index];

            let channel_count = output.channel_count() as usize;
            if channel_count != data.end - data.start {
                return Err(PluginError::Message("Channel count mismatch"));
            }

            let channels = output.channels()?.into_f32().ok_or(PluginError::Message(
                "Expected f32 (float) output audio channels",
            ))?;

            self.buffer_ptrs[data.start..data.end].copy_from_slice(channels.raw_data());
        }

        for (&bus_index, input) in zip(&self.instance.input_bus_map, audio.input_ports()) {
            let data = &self.buffer_data[bus_index];
            let bus_info = &self.instance.info.buses[bus_index];

            let channel_count = input.channel_count() as usize;
            if channel_count != data.end - data.start {
                return Err(PluginError::Message("Channel count mismatch"));
            }

            let channels = input.channels()?.into_f32().ok_or(PluginError::Message(
                "Expected f32 (float) input audio channels",
            ))?;

            let ptrs = &mut self.buffer_ptrs[data.start..data.end];

            match bus_info.dir {
                BusDir::In => {
                    ptrs.copy_from_slice(channels.raw_data());
                }
                BusDir::InOut => unsafe {
                    for (src, &mut dst) in zip(channels, ptrs) {
                        if src.as_ptr() != dst {
                            let dst = slice::from_raw_parts_mut(dst, len);
                            dst.copy_from_slice(src);
                        }
                    }
                },
                BusDir::Out => unreachable!(),
            }
        }

        self.events.clear();

        for event in events.input {
            let Some(event) = event.as_core_event() else {
                continue;
            };

            if let CoreEventSpace::ParamValue(event) = event {
                if let Some(&index) = self.instance.param_map.get(&event.param_id()) {
                    let value = map_param_in(&self.instance.info.params[index], event.value());

                    self.events.push(Event {
                        time: event.header().time() as i64,
                        data: Data::ParamChange {
                            id: event.param_id(),
                            value,
                        },
                    });

                    self.instance.plugin_params.set(index, value);
                }
            }
        }

        self.instance.sync_processor(&mut self.processor);

        self.processor.process(
            unsafe {
                Buffers::from_raw_parts(
                    RawBuffers {
                        buffers: &self.buffer_data,
                        ptrs: &self.buffer_ptrs,
                        offset: 0,
                    },
                    len,
                )
            },
            Events::new(&self.events),
        );

        Ok(ProcessStatus::Continue)
    }
}

impl<'a, P: Plugin> PluginAudioPortsImpl for MainThreadState<'a, P> {
    fn count(&mut self, is_input: bool) -> u32 {
        if is_input {
            self.instance.input_bus_map.len() as u32
        } else {
            self.instance.output_bus_map.len() as u32
        }
    }

    fn get(&mut self, is_input: bool, index: u32, writer: &mut AudioPortInfoWriter) {
        let bus_index = if is_input {
            self.instance.input_bus_map.get(index as usize)
        } else {
            self.instance.output_bus_map.get(index as usize)
        };

        if let Some(&bus_index) = bus_index {
            let bus_info = self.instance.info.buses.get(bus_index);

            let layout = &self.instance.info.layouts[self.layout_index];
            let format = layout.formats.get(bus_index);

            if let (Some(bus_info), Some(format)) = (bus_info, format) {
                writer.set(&AudioPortInfoData {
                    id: index,
                    name: bus_info.name.as_bytes(),
                    channel_count: 0,
                    flags: if index == 0 {
                        AudioPortFlags::IS_MAIN
                    } else {
                        AudioPortFlags::empty()
                    },
                    port_type: Some(port_type_from_format(format)),
                    in_place_pair: if bus_info.dir == BusDir::InOut {
                        // Find the other half of this input-output pair
                        let bus_map = if is_input {
                            &self.instance.output_bus_map
                        } else {
                            &self.instance.input_bus_map
                        };

                        bus_map.iter().position(|&i| i == bus_index).map(|i| i as u32)
                    } else {
                        None
                    },
                });
            }
        }
    }
}

impl<'a, P: Plugin> PluginAudioPortsConfigImpl for MainThreadState<'a, P> {
    fn count(&mut self) -> u32 {
        self.instance.info.layouts.len() as u32
    }

    fn get(&mut self, index: u32, writer: &mut AudioPortConfigWriter) {
        let instance = self.instance;

        if let Some(layout) = instance.info.layouts.get(index as usize) {
            writer.write(&AudioPortsConfiguration {
                id: index,
                name: b"",
                input_port_count: instance.input_bus_map.len() as u32,
                output_port_count: instance.output_bus_map.len() as u32,
                main_input: if let Some(&bus_index) = instance.input_bus_map.first() {
                    let format = &layout.formats[bus_index];

                    Some(MainPortInfo {
                        channel_count: format.channel_count() as u32,
                        port_type: Some(port_type_from_format(format)),
                    })
                } else {
                    None
                },
                main_output: if let Some(&bus_index) = instance.output_bus_map.first() {
                    let format = &layout.formats[bus_index];

                    Some(MainPortInfo {
                        channel_count: format.channel_count() as u32,
                        port_type: Some(port_type_from_format(format)),
                    })
                } else {
                    None
                },
            })
        }
    }

    fn select(&mut self, config_id: u32) -> Result<(), AudioPortConfigSelectError> {
        if self.instance.info.layouts.get(config_id as usize).is_some() {
            self.layout_index = config_id as usize;
            Ok(())
        } else {
            Err(AudioPortConfigSelectError)
        }
    }
}

impl<'a, P: Plugin> PluginMainThreadParams for MainThreadState<'a, P> {
    fn count(&mut self) -> u32 {
        self.instance.info.params.len() as u32
    }

    fn get_info(&mut self, param_index: u32, writer: &mut ParamInfoWriter) {
        if let Some(param) = self.instance.info.params.get(param_index as usize) {
            let mut info = ParamInfoData {
                id: param.id,
                flags: ParamInfoFlags::IS_AUTOMATABLE,
                cookie: Cookie::empty(),
                name: &param.name,
                module: "",
                min_value: 0.0,
                max_value: 1.0,
                default_value: map_param_out(&param, param.default),
            };

            if let Some(steps) = param.steps {
                info.flags |= ParamInfoFlags::IS_STEPPED;
                info.min_value = 0.0;
                info.max_value = (steps.max(2) - 1) as f64;
            }

            writer.set(&info);
        }
    }

    fn get_value(&mut self, param_id: u32) -> Option<f64> {
        let &index = self.instance.param_map.get(&param_id)?;
        self.instance.sync_plugin(&mut self.plugin);

        let param = &self.instance.info.params[index];
        Some(map_param_out(param, self.plugin.get_param(param_id)))
    }

    fn value_to_text(
        &mut self,
        param_id: u32,
        value: f64,
        writer: &mut ParamDisplayWriter,
    ) -> std::fmt::Result {
        use std::fmt::Write;

        let &index = self.instance.param_map.get(&param_id).ok_or(std::fmt::Error)?;
        let param = &self.instance.info.params[index];

        write!(
            writer,
            "{}",
            DisplayParam(param, map_param_in(param, value))
        )
    }

    fn text_to_value(&mut self, param_id: u32, text: &str) -> Option<f64> {
        let &index = self.instance.param_map.get(&param_id)?;
        let param = &self.instance.info.params[index];

        (param.parse)(text)
    }

    fn flush(
        &mut self,
        input_parameter_changes: &InputEvents,
        _output_parameter_changes: &mut OutputEvents,
    ) {
        self.instance.sync_plugin(&mut self.plugin);

        for event in input_parameter_changes {
            let Some(event) = event.as_core_event() else {
                continue;
            };

            if let CoreEventSpace::ParamValue(event) = event {
                if let Some(&index) = self.instance.param_map.get(&event.param_id()) {
                    let value = map_param_in(&self.instance.info.params[index], event.value());
                    self.plugin.set_param(event.param_id(), value);
                    self.instance.processor_params.set(index, value);
                }
            }
        }
    }
}

impl<'a, P: Plugin> PluginAudioProcessorParams for ProcessState<'a, P> {
    fn flush(
        &mut self,
        input_parameter_changes: &InputEvents,
        _output_parameter_changes: &mut OutputEvents,
    ) {
        self.instance.sync_processor(&mut self.processor);

        for event in input_parameter_changes {
            let Some(event) = event.as_core_event() else {
                continue;
            };

            if let CoreEventSpace::ParamValue(event) = event {
                if let Some(&index) = self.instance.param_map.get(&event.param_id()) {
                    let value = map_param_in(&self.instance.info.params[index], event.value());
                    self.processor.set_param(event.param_id(), value);
                    self.instance.plugin_params.set(index, value);
                }
            }
        }
    }
}

impl<'a, P: Plugin> PluginStateImpl for MainThreadState<'a, P> {
    fn save(&mut self, output: &mut OutputStream) -> Result<(), PluginError> {
        self.instance.sync_plugin(&mut self.plugin);
        self.plugin.save(output)?;

        Ok(())
    }

    fn load(&mut self, input: &mut InputStream) -> Result<(), PluginError> {
        self.instance.sync_plugin(&mut self.plugin);
        self.plugin.load(input)?;

        for (index, param) in self.instance.info.params.iter().enumerate() {
            let value = self.plugin.get_param(param.id);
            self.instance.processor_params.set(index, value);
        }

        Ok(())
    }
}
