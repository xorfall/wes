//! Default is an explicit provider namespace, not a fallback. Existing captured-import admission
//! owns its extensions: prepare a complete image, journal the source, then publish atomically.
use super::*;
use crate::environments::{Authority, ExecutionImages, Registry};
use std::collections::BTreeMap;
use wes_core::environments::{ConfiguredProvider, EffectiveEnvironment, Package, Revision};

#[derive(Clone, Debug)]
pub(super) struct DefaultEnvironment {
    pub name: String,
    products: BTreeMap<String, Arc<ImportProduct>>,
    evidence: BTreeMap<String, ConfiguredProvider>,
    authority: Option<Authority>,
    captured: BTreeMap<String, CapturedImport>,
}
pub(super) struct Publication {
    pub configuration: DefaultEnvironment,
    pub registry: Registry,
    pub images: ExecutionImages,
}
pub(super) struct DocumentPublication {
    pub environment: String,
    pub alias: String,
    pub record: crate::environments::EnvironmentRecord,
    pub registry: Registry,
    pub images: ExecutionImages,
}
impl std::fmt::Debug for DocumentPublication {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvironmentImportPublication")
            .field("record", &self.record)
            .finish_non_exhaustive()
    }
}
pub(super) fn import_document(
    current: &crate::environments::EnvironmentRecord,
    loader: &dyn crate::environments::EnvironmentLoader,
    name: &str,
    captured: &CapturedImport,
    registry: &Registry,
    images: &ExecutionImages,
    history_bytes: u64,
) -> Result<DocumentPublication, WorkspaceError> {
    let loaded = loader
        .capture_import(current, name, captured)
        .map_err(reject)?;
    let (plan, record) =
        crate::environments::EnvironmentRecord::capture(loaded.yaml, loaded.sources, registry)
            .map_err(reject)?;
    if history_bytes.saturating_add(record.charge()) > 64 * 1024 * 1024 {
        return Err(rejected(
            "ENV010",
            Span::at(0),
            "Environment history exceeds its byte budget",
        ));
    }
    let images = images.build(&plan, loader).map_err(reject)?;
    let mut registry = registry.planning_snapshot();
    registry.apply(plan).map_err(reject)?;
    Ok(DocumentPublication {
        environment: name.into(),
        alias: captured.product().description().name().into(),
        record,
        registry,
        images,
    })
}
impl std::fmt::Debug for Publication {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultEnvironmentPublication")
            .field("registry", &self.registry)
            .finish_non_exhaustive()
    }
}
fn reject(e: wes_core::environments::EnvironmentError) -> WorkspaceError {
    rejected(e.code, Span::at(0), e.message)
}
impl DefaultEnvironment {
    pub fn new(
        name: &str,
        products: Vec<Arc<ImportProduct>>,
        authority: Option<Authority>,
    ) -> Result<Self, WorkspaceError> {
        let mut configuration = Self {
            name: name.into(),
            products: BTreeMap::new(),
            evidence: BTreeMap::new(),
            authority,
            captured: BTreeMap::new(),
        };
        for product in products {
            let alias = product.description().name().to_owned();
            if configuration.products.contains_key(&alias) {
                return Err(rejected(
                    "ENV001",
                    Span::at(0),
                    "duplicate configured provider",
                ));
            }
            configuration.evidence.insert(
                alias.clone(),
                ConfiguredProvider {
                    endpoint: None,
                    evidence: Revision::evidence(
                        "wes-builtins/v1",
                        [product.description().revision_evidence().as_str()],
                    )
                    .to_string(),
                },
            );
            configuration.products.insert(alias, product);
        }
        Ok(configuration)
    }
    pub(super) fn captures(&self) -> impl Iterator<Item = (&str, &CapturedImport)> {
        self.captured
            .iter()
            .map(|(alias, captured)| (alias.as_str(), captured))
    }
    pub fn imported(
        &self,
        captured: &CapturedImport,
        registry: &Registry,
        images: &ExecutionImages,
    ) -> Result<Publication, WorkspaceError> {
        let mut configuration = self.clone();
        configuration.captured.insert(
            captured.product().description().name().into(),
            captured.clone(),
        );
        let product = captured.product();
        let alias = product.description().name().to_owned();
        let recipe = captured.recipe();
        let request = captured.request();
        let arguments = request
            .arguments()
            .iter()
            .map(|(name, value)| (name, (value.shape(), value.data())))
            .collect::<BTreeMap<_, _>>();
        let request_evidence = format!("{:?}", (request.kind(), request.alias(), arguments));
        let metadata = product.description().revision_evidence();
        configuration.evidence.insert(
            alias.clone(),
            ConfiguredProvider {
                endpoint: request.arguments().get("endpoint").and_then(|value| {
                    match value.data() {
                        wes_core::Data::Text(endpoint) => Some(endpoint.to_string()),
                        _ => None,
                    }
                }),
                evidence: Revision::evidence(
                    "wes-captured-provider/v1",
                    [
                        recipe.format(),
                        recipe.source(),
                        &request_evidence,
                        &metadata,
                    ],
                )
                .to_string(),
            },
        );
        configuration.products.insert(alias, product.clone());
        configuration.prepare(registry, images)
    }
    pub fn prepare(
        self,
        registry: &Registry,
        images: &ExecutionImages,
    ) -> Result<Publication, WorkspaceError> {
        let (package, sources) =
            Package::configured_providers(&self.name, self.evidence.clone()).map_err(reject)?;
        let environment = Arc::new(
            EffectiveEnvironment::resolve(&package, &self.name, None, &sources).map_err(reject)?,
        );
        let mut providers = Providers::new();
        for (alias, product) in &self.products {
            providers
                .register_environment(
                    product,
                    environment.bind(alias).map_err(reject)?,
                    self.authority.clone(),
                )
                .map_err(|error| rejected("ENV010", Span::at(0), error.to_string()))?;
        }
        let mut registry = registry.planning_snapshot();
        registry.configure(environment.clone()).map_err(reject)?;
        let mut images = images.clone();
        images
            .configure(
                &environment,
                providers,
                self.products.values().map(|p| p.charge()).sum(),
            )
            .map_err(reject)?;
        Ok(Publication {
            configuration: self,
            registry,
            images,
        })
    }
    pub(crate) fn document(
        &self,
        loader: &dyn crate::environments::EnvironmentLoader,
    ) -> Result<crate::environments::LoadedDefinitions, wes_core::environments::EnvironmentError>
    {
        loader.default_document(
            &self.name,
            &self.captured.values().cloned().collect::<Vec<_>>(),
        )
    }
}
