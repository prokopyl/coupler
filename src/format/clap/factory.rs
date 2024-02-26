use std::ffi::CStr;
use std::marker::PhantomData;
use std::sync::Arc;

use clack_plugin::entry::prelude::*;

use super::instance::{Instance, InstanceShared, MainThreadState};
use super::ClapPlugin;
use crate::plugin::{Plugin, PluginInfo};

struct ClapFactory<P> {
    descriptor: PluginDescriptor,
    info: Arc<PluginInfo>,
    _marker: PhantomData<fn() -> P>,
}

#[doc(hidden)]
pub struct ClapEntry<P> {
    factory: PluginFactoryWrapper<ClapFactory<P>>,
}

impl<P: Plugin + ClapPlugin> Entry for ClapEntry<P> {
    fn new(_bundle_path: &CStr) -> Result<Self, EntryLoadError> {
        Ok(Self {
            factory: PluginFactoryWrapper::new(ClapFactory::new()),
        })
    }

    fn declare_factories<'a>(&'a self, builder: &mut EntryFactories<'a>) {
        builder.register_factory(&self.factory);
    }
}

impl<P: Plugin + ClapPlugin> ClapFactory<P> {
    pub fn new() -> Self {
        let info = Arc::new(P::info());
        let clap_info = P::clap_info();

        let descriptor = PluginDescriptor::new(&clap_info.id, &info.name)
            .with_vendor(&info.vendor)
            .with_url(&info.url)
            .with_version(&info.version);

        Self {
            info,
            descriptor,
            _marker: PhantomData,
        }
    }
}

impl<P: Plugin + ClapPlugin> PluginFactory for ClapFactory<P> {
    fn plugin_count(&self) -> u32 {
        1
    }

    fn plugin_descriptor(&self, index: u32) -> Option<&PluginDescriptor> {
        match index {
            0 => Some(&self.descriptor),
            _ => None,
        }
    }

    fn create_plugin<'a>(
        &'a self,
        host_info: HostInfo<'a>,
        plugin_id: &CStr,
    ) -> Option<PluginInstance<'a>> {
        if plugin_id == self.descriptor.id() {
            Some(PluginInstance::new::<Instance<P>>(
                host_info,
                &self.descriptor,
                |host| InstanceShared::new(host, &self.info),
                |host, instance| MainThreadState::new(host, instance),
            ))
        } else {
            None
        }
    }
}
