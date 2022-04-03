use crate::{buffer::*, bus::*, param::*, plugin::*};

pub struct ProcessContext<'a> {
    sample_rate: f64,
    input_layouts: &'a [BusLayout],
    output_layouts: &'a [BusLayout],
    param_states: &'a ParamStates,
}

impl<'a> ProcessContext<'a> {
    pub fn new(
        sample_rate: f64,
        input_layouts: &'a [BusLayout],
        output_layouts: &'a [BusLayout],
        param_states: &'a ParamStates,
    ) -> ProcessContext<'a> {
        ProcessContext { sample_rate, input_layouts, output_layouts, param_states }
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    pub fn input_layouts(&self) -> &'a [BusLayout] {
        self.input_layouts
    }

    pub fn output_layouts(&self) -> &'a [BusLayout] {
        self.output_layouts
    }

    pub fn param_list(&self) -> &ParamList {
        &self.param_states.list
    }

    #[inline]
    pub fn get_param<P: Param + 'static>(&self, key: ParamKey<P>) -> P::Value {
        self.param_states.get_param(key)
    }

    #[inline]
    pub fn read_change<P: Param + 'static>(
        &self,
        key: ParamKey<P>,
        change: ParamChange,
    ) -> Option<P::Value> {
        self.param_states.list.read_change(key, change)
    }
}

pub struct Event {
    pub offset: usize,
    pub event: EventType,
}

pub enum EventType {
    ParamChange(ParamChange),
}

pub trait Processor: Send + Sized {
    type Plugin: Plugin;

    fn create(plugin: &Self::Plugin, context: &ProcessContext) -> Self;
    fn reset(&mut self, context: &ProcessContext);
    fn process(&mut self, context: &ProcessContext, buffers: &mut Buffers, events: &[Event]);
}
