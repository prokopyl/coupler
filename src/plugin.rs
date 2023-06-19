use crate::{bus::*, editor::*, param::*, process::*};

#[derive(Clone)]
pub struct PluginInfo {
    name: String,
    vendor: String,
    url: String,
    email: String,
    has_editor: bool,
}

impl Default for PluginInfo {
    #[inline]
    fn default() -> PluginInfo {
        PluginInfo {
            name: String::new(),
            vendor: String::new(),
            url: String::new(),
            email: String::new(),
            has_editor: false,
        }
    }
}

impl PluginInfo {
    #[inline]
    pub fn new() -> PluginInfo {
        Self::default()
    }

    #[inline]
    pub fn name(mut self, name: &str) -> Self {
        self.name = name.to_string();
        self
    }

    #[inline]
    pub fn vendor(mut self, vendor: &str) -> Self {
        self.vendor = vendor.to_string();
        self
    }

    #[inline]
    pub fn url(mut self, url: &str) -> Self {
        self.url = url.to_string();
        self
    }

    #[inline]
    pub fn email(mut self, email: &str) -> Self {
        self.email = email.to_string();
        self
    }

    #[inline]
    pub fn has_editor(mut self, has_editor: bool) -> Self {
        self.has_editor = has_editor;
        self
    }

    #[inline]
    pub fn get_name(&self) -> &str {
        &self.name
    }

    #[inline]
    pub fn get_vendor(&self) -> &str {
        &self.vendor
    }

    #[inline]
    pub fn get_url(&self) -> &str {
        &self.url
    }

    #[inline]
    pub fn get_email(&self) -> &str {
        &self.email
    }

    #[inline]
    pub fn get_has_editor(&self) -> bool {
        self.has_editor
    }
}

pub trait Plugin: Send + Sync + Sized + 'static {
    type Processor: Processor<Self>;
    type Editor: Editor<Self>;

    fn info() -> PluginInfo;
    fn buses() -> BusList;
    fn bus_configs() -> BusConfigList;
    fn params() -> ParamList<Self>;
    fn create() -> Self;
    fn serialize(&self, write: &mut impl std::io::Write) -> Result<(), ()>;
    fn deserialize(&self, read: &mut impl std::io::Read) -> Result<(), ()>;
}
