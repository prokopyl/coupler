use std::slice;

#[derive(Eq, PartialEq, Clone)]
pub enum BusLayout {
    Stereo,
}

impl BusLayout {
    pub fn channels(&self) -> usize {
        match self {
            BusLayout::Stereo => 2,
        }
    }
}

pub struct AudioBuffers<'a, 'b> {
    pub(crate) samples: usize,
    pub(crate) inputs: AudioBuses<'a, 'b>,
    pub(crate) outputs: AudioBuses<'a, 'b>,
}

impl<'a, 'b> AudioBuffers<'a, 'b> {
    pub fn samples(&self) -> usize {
        self.samples
    }

    pub fn inputs(&self) -> &AudioBuses<'a, 'b> {
        &self.inputs
    }

    pub fn outputs(&mut self) -> &mut AudioBuses<'a, 'b> {
        &mut self.outputs
    }
}

pub struct AudioBuses<'a, 'b> {
    pub(crate) samples: usize,
    pub(crate) buses: &'a mut [AudioBus<'b>],
}

impl<'a, 'b> AudioBuses<'a, 'b> {
    pub fn samples(&self) -> usize {
        self.samples
    }

    pub fn buses(&self) -> usize {
        self.buses.len()
    }

    pub fn bus(&self, index: usize) -> Option<&AudioBus<'b>> {
        self.buses.get(index)
    }

    pub fn bus_mut(&mut self, index: usize) -> Option<&mut AudioBus<'b>> {
        self.buses.get_mut(index)
    }

    pub fn iter(&self) -> impl Iterator<Item = &AudioBus<'b>> {
        self.buses.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut AudioBus<'b>> {
        self.buses.iter_mut()
    }
}

pub struct AudioBus<'a> {
    pub(crate) layout: &'a BusLayout,
    pub(crate) samples: usize,
    pub(crate) channels: Option<&'a [*mut f32]>,
}

impl<'a> AudioBus<'a> {
    pub fn enabled(&self) -> bool {
        self.channels.is_some()
    }

    pub fn layout(&self) -> &BusLayout {
        self.layout
    }

    pub fn samples(&self) -> usize {
        self.samples
    }

    pub fn channels(&self) -> usize {
        if let Some(channels) = self.channels {
            channels.len()
        } else {
            0
        }
    }

    pub fn channel(&self, index: usize) -> Option<&[f32]> {
        if let Some(channels) = self.channels {
            if let Some(&channel) = channels.get(index) {
                return Some(unsafe { slice::from_raw_parts(channel, self.samples) });
            }
        }

        None
    }

    pub fn channel_mut(&mut self, index: usize) -> Option<&mut [f32]> {
        if let Some(channels) = self.channels {
            if let Some(&channel) = channels.get(index) {
                return Some(unsafe { slice::from_raw_parts_mut(channel, self.samples) });
            }
        }

        None
    }

    pub fn iter(&self) -> impl Iterator<Item = &[f32]> {
        let samples = self.samples;
        let channels = self.channels.unwrap_or(&[]);
        channels.iter().map(move |channel| unsafe { slice::from_raw_parts(*channel, samples) })
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut [f32]> {
        let samples = self.samples;
        let channels = self.channels.unwrap_or(&mut []);
        channels.iter().map(move |channel| unsafe { slice::from_raw_parts_mut(*channel, samples) })
    }
}
