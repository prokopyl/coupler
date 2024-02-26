mod factory;
mod gui;
mod instance;

pub struct ClapInfo {
    pub id: String,
}

pub trait ClapPlugin {
    fn clap_info() -> ClapInfo;
}

#[doc(hidden)]
pub mod __internal {
    pub use super::factory::ClapEntry;
    pub use clack_plugin::prelude::clack_export_entry;
}

#[macro_export]
macro_rules! clap {
    ($plugin:ty) => {
        const _: () = {
            use $crate::format::clap::__internal::*;
            clack_export_entry!(ClapEntry<$plugin>);
        };
    };
}
