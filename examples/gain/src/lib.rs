use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

use coupler::format::clap::*;
// use coupler::format::vst3::*;
use coupler::{buffers::*, bus::*, editor::*, events::*, params::*, plugin::*, process::*};

#[derive(Params, Serialize, Deserialize, Clone)]
struct GainParams {
    #[param(id = 0, name = "Gain", range = 0.0..1.0, format = "{:.2}")]
    gain: f32,
}

impl Default for GainParams {
    fn default() -> GainParams {
        GainParams { gain: 1.0 }
    }
}

pub struct Gain {
    params: GainParams,
}

impl Plugin for Gain {
    type Processor = GainProcessor;
    type Editor = NoEditor;

    fn info() -> PluginInfo {
        PluginInfo {
            name: "Gain".to_string(),
            version: "0.1.0".to_string(),
            vendor: "Vendor".to_string(),
            url: "https://example.com".to_string(),
            email: "example@example.com".to_string(),
            buses: vec![BusInfo {
                name: "Main".to_string(),
                dir: BusDir::InOut,
            }],
            layouts: vec![
                Layout {
                    formats: vec![Format::Stereo],
                },
                Layout {
                    formats: vec![Format::Mono],
                },
            ],
            params: GainParams::params(),
            has_editor: false,
        }
    }

    fn new(_host: Host) -> Self {
        Gain {
            params: GainParams::default(),
        }
    }

    fn set_param(&mut self, id: ParamId, value: ParamValue) {
        self.params.set_param(id, value);
    }

    fn get_param(&self, id: ParamId) -> ParamValue {
        self.params.get_param(id)
    }

    fn save(&self, output: &mut impl Write) -> io::Result<()> {
        serde_json::to_writer(output, &self.params)?;

        Ok(())
    }

    fn load(&mut self, input: &mut impl Read) -> io::Result<()> {
        self.params = serde_json::from_reader(input)?;

        Ok(())
    }

    fn processor(&self, config: Config) -> Self::Processor {
        GainProcessor {
            params: self.params.clone(),
        }
    }

    fn editor(&self, _parent: Parent) -> Self::Editor {
        NoEditor
    }
}
/*
impl Vst3Plugin for Gain {
    fn vst3_info() -> Vst3Info {
        Vst3Info {
            class_id: Uuid(0x84B4DD04, 0x0D964565, 0x97AC3AAA, 0x87C5CCA7),
        }
    }
}*/

impl ClapPlugin for Gain {
    fn clap_info() -> ClapInfo {
        ClapInfo {
            id: "rs.coupler.gain".to_string(),
        }
    }
}

pub struct GainProcessor {
    params: GainParams,
}

impl Processor for GainProcessor {
    fn set_param(&mut self, id: ParamId, value: ParamValue) {
        self.params.set_param(id, value);
    }

    fn reset(&mut self) {}

    fn process(&mut self, buffers: Buffers, events: Events) {
        let buffer: BufferMut = buffers.try_into().unwrap();
        for (buffer, events) in buffer.split_at_events(events) {
            for event in events {
                match event.data {
                    Data::ParamChange { id, value } => {
                        self.params.set_param(id, value);
                    }
                    _ => {}
                }
            }

            for sample in buffer.samples() {
                for channel in sample {
                    *channel *= self.params.gain;
                }
            }
        }
    }
}
