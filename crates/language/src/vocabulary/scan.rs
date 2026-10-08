//! Closed finite-analysis admission vocabulary. Unimplemented store/live modes
//! are not offered as choices by help, completion or agent discovery.
use wes_core::{Primitive, Shape, capability::Parameter, contracts::ContractRegistry};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    LinesUtf8,
    LinesLossyUtf8,
    DelimitedUtf8,
    TypedRecords,
}
impl Profile {
    pub const ALL: &[Self] = &[
        Self::LinesUtf8,
        Self::LinesLossyUtf8,
        Self::DelimitedUtf8,
        Self::TypedRecords,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::LinesUtf8 => "LinesUtf8",
            Self::LinesLossyUtf8 => "LinesLossyUtf8",
            Self::DelimitedUtf8 => "DelimitedUtf8",
            Self::TypedRecords => "TypedRecords",
        }
    }
    pub fn lookup(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|profile| profile.name() == name)
    }
}
pub fn parameters() -> Vec<Parameter> {
    let mut registry = ContractRegistry::new();
    registry.load("types:\n  ScanProfile: {base: Text, enum: [LinesUtf8, LinesLossyUtf8, DelimitedUtf8, TypedRecords]}\n  ScanMode: {base: Text, enum: [complete]}\n  ScanSink: {base: Text, enum: [memory]}\n  ScanBudget: {base: Text, enum: [Investigation]}").expect("finite scan vocabulary");
    let text = Shape::Primitive(Primitive::Text);
    let choice = |name: &str, type_name: &str, required: bool| {
        Parameter::new(name, text.clone(), required).constrained_by(
            &registry
                .resolve(type_name)
                .expect("finite scan choice contract"),
        )
    };
    vec![
        Parameter::new("source", Shape::Unknown, true),
        Parameter::new("transition", text.clone(), true).selecting("template"),
        Parameter::new("finish", text.clone(), false).selecting("template"),
        Parameter::new("initial", Shape::Unknown, true),
        Parameter::new("context", Shape::Unknown, true),
        choice("profile", "ScanProfile", true),
        Parameter::new("delimiter", text.clone(), false),
        choice("budget", "ScanBudget", false),
        choice("mode", "ScanMode", false),
        choice("sink", "ScanSink", false),
    ]
}
