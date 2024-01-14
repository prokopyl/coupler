use std::fmt::{self, Formatter};

use crate::{ParamId, ParamValue};

pub type ParseFn = dyn Fn(&str) -> Option<ParamValue> + Send + Sync;
pub type DisplayFn = dyn Fn(ParamValue, &mut Formatter) -> Result<(), fmt::Error> + Send + Sync;

pub struct ParamInfo {
    pub id: ParamId,
    pub name: String,
    pub default: ParamValue,
    pub steps: Option<u32>,
    pub parse: Box<ParseFn>,
    pub display: Box<DisplayFn>,
}
