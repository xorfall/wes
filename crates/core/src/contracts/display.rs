//! Declared presentation hints. These never participate in validation or subtyping.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EnumTone {
    Ok,
    Warn,
    Bad,
    Dim,
    Meta,
    Ink,
}
impl EnumTone {
    pub const NAMES: &'static [&'static str] = &["ok", "warn", "bad", "dim", "meta", "ink"];
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "ok" => Self::Ok,
            "warn" => Self::Warn,
            "bad" => Self::Bad,
            "dim" => Self::Dim,
            "meta" => Self::Meta,
            "ink" => Self::Ink,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Bad => "bad",
            Self::Dim => "dim",
            Self::Meta => "meta",
            Self::Ink => "ink",
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ContractDisplay {
    #[serde(rename = "enumTones")]
    pub(super) enum_tones: BTreeMap<String, EnumTone>,
}
impl ContractDisplay {
    pub fn enum_tones(&self) -> &BTreeMap<String, EnumTone> {
        &self.enum_tones
    }
}
