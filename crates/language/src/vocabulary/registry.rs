//! The closed list-query vocabulary. Scope and filters belong to a registry's type.
use wes_core::{Primitive, Shape, capability::Parameter};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistryScope {
    Catalogue,
    Workspace,
    Home,
    Language,
}
impl RegistryScope {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Catalogue => "catalogue",
            Self::Workspace => "workspace",
            Self::Home => "home",
            Self::Language => "language",
        }
    }
}

// One declaration generates the complete domain and its total metadata functions.
macro_rules! registries {
    ($($kind:ident => ($name:literal, $scope:ident, $summary:literal, $provider:literal)),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum ListRegistry { $($kind),+ }
        impl ListRegistry {
            pub const ALL: &[Self] = &[$(Self::$kind),+];
            pub const fn name(self) -> &'static str { match self { $(Self::$kind => $name),+ } }
            pub const fn scope(self) -> RegistryScope { match self { $(Self::$kind => RegistryScope::$scope),+ } }
            pub const fn summary(self) -> &'static str { match self { $(Self::$kind => $summary),+ } }
            pub fn parameters(self) -> Vec<Parameter> {
                let provider = match self { $(Self::$kind => $provider),+ };
                if provider {
                    vec![Parameter::new("provider", Shape::Primitive(Primitive::Text), false).selecting("provider")]
                } else { vec![] }
            }
            pub fn lookup(name: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|registry| registry.name() == name)
            }
        }
    }
}
registries! {
    Homes => ("homes", Home, "Data-home identities attached to this engine context; no filesystem discovery.", false),
    Sandboxes => ("sandboxes", Workspace, "Saved sandbox definition names in this workspace; listing does not start their programs.", false),
    Cells => ("cells", Workspace, "Session cell metadata without source text.", false),
    Runs => ("runs", Workspace, "Latest retained run metadata for current graph nodes.", false),
    Environments => ("environments", Home, "Managed environment definitions in the attached data home.", false),
    Templates => ("templates", Workspace, "Declared command-template names.", false),
    Adapters => ("adapters", Workspace, "Pure typed calculation definitions with an explicit input parameter and output contract. Inspect template:<name> for captured revision, mapping and input/output types; using an adapter is ordinary calculation work.", false),
    Views => ("views", Workspace, "Built-in and installed view package definitions in this workspace; inspect view:<name> describes input contracts, state/event outputs and whether they are shared. No renderer or provider runs.", false),
    Types => ("types", Workspace, "Effective type vocabulary: workspace contracts and language constructors; rows identify origin/kind.", false),
    Providers => ("providers", Catalogue, "Provider names in the captured effective catalogue.", false),
    Capabilities => ("capabilities", Catalogue, "Capability signatures; provider filters rows by exact provider name.", true),
    Names => ("names", Workspace, "Current result-name bindings, including state and selected output port.", false),
    Nodes => ("nodes", Workspace, "Current graph-node metadata, without result bodies.", false),
    Importers => ("importers", Workspace, "Registered importer names.", false),
    Commands => ("commands", Language, "Canonical command roots; use help for child operations and supported short forms. Not an authorization list.", false),
    Workspaces => ("workspaces", Home, "Saved workspace names in the attached data home; not other homes or agent memberships.", false),
}

/// Shared human/machine rule; individual registry meanings come from the declaration above.
pub const LIST_SEMANTICS: &str = "List a declared registry at execution entry. Scope is registry metadata. Catalogue means the captured selected environment, or the workspace catalogue when unmanaged; no selected environment means an empty catalogue. Filters preserve order; no matches return []. Unsupported arguments fail. Size/work limits fail without partial results. Listing does not invoke providers or read result bodies; normal execution still records a cell/node.";

/// Shared value-shaped documentation for engine help and external adapters.
pub fn list_metadata() -> wes_core::Data {
    list_metadata_for(ListRegistry::ALL)
}

pub fn list_metadata_for(selected: &[ListRegistry]) -> wes_core::Data {
    use wes_core::Data;
    let record = |entries: Vec<(&str, Data)>| {
        Data::Record(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect(),
        )
    };
    let registries = selected
        .iter()
        .map(|registry| {
            record(vec![
                ("name", Data::Text(registry.name().into())),
                ("scope", Data::Text(registry.scope().name().into())),
                ("summary", Data::Text(registry.summary().into())),
                (
                    "filters",
                    Data::List(
                        registry
                            .parameters()
                            .iter()
                            .map(|p| Data::Text(p.name.as_str().into()))
                            .collect(),
                    ),
                ),
            ])
        })
        .collect();
    record(vec![
        ("rule", Data::Text(LIST_SEMANTICS.into())),
        ("registries", Data::List(registries)),
    ])
}
