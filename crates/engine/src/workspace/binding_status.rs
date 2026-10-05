//! Captured destinations never follow a mutable provider alias. Admission reads current definitions.
use super::*;
use wes_core::environments::{Binding, EnvironmentError};

pub(crate) const BINDING_CHANGED: &str = "ENV039";

impl Workspace {
    fn binding_current(&self, captured: &Binding) -> bool {
        let current = self
            .environments
            .inspect(captured.environment().name())
            .filter(|environment| environment.identity() == captured.environment().identity())
            .or_else(|| {
                self.environments.names().find_map(|name| {
                    self.environments.inspect(name).filter(|environment| {
                        environment.identity() == captured.environment().identity()
                    })
                })
            });
        current.is_some_and(|current| {
            current.revision() == captured.environment().revision()
                || current
                    .bind(captured.alias())
                    .is_ok_and(|binding| captured.import().same_execution(binding.import()))
        })
    }

    pub(crate) fn check_task_bindings(&self, task: &BoundTask) -> Result<(), EnvironmentError> {
        for binding in task.environments() {
            if !self.binding_current(binding) {
                return Err(EnvironmentError {
                    code: BINDING_CHANGED,
                    message: format!(
                        "Captured binding for provider '{}' in environment '{}' changed or was removed (captured destination: {}, rev {}). No provider was called. Existing cells keep their original destination. Submit a new command or use New branch to resolve the current binding; inspect the original result to review its captured binding.",
                        binding.alias(),
                        binding.environment().name(),
                        destination(binding),
                        short_revision(binding),
                    ),
                });
            }
        }
        Ok(())
    }

    /// Explicit refresh validates the entire potentially reactive closure before changing any run.
    pub(crate) fn check_refresh_bindings(
        &self,
        targets: &[NodeId],
        span: Span,
    ) -> Result<(), WorkspaceError> {
        let graph = self.runtime.graph();
        let mut selected = std::collections::BTreeSet::new();
        let mut pending = targets.to_vec();
        while let Some(id) = pending.pop() {
            if !selected.insert(id.clone()) {
                continue;
            }
            if graph.node(&id).is_none() {
                return Err(RuntimeError::from(crate::graph::GraphError::Missing(id)).into());
            }
            pending.extend(graph.dependents_of(&id).cloned());
        }
        for id in selected {
            let node = self.runtime.graph().node(&id).expect("validated closure");
            self.check_task_bindings(node.payload())
                .map_err(|error| rejected(error.code, span, error.message))?;
        }
        Ok(())
    }

    /// Only destination origin is projected: no URL credentials, paths, query or fragment.
    pub(crate) fn captured_bindings(&self, task: &BoundTask) -> Vec<String> {
        task.environments()
            .map(|binding| {
                format!(
                    "{} · {}",
                    binding_description(binding, false),
                    if self.binding_current(binding) {
                        "current binding"
                    } else {
                        "binding changed or removed; new command required"
                    }
                )
            })
            .collect()
    }

    pub(crate) fn refresh_binding_summary(&self, targets: &[NodeId]) -> String {
        let mut summaries = std::collections::BTreeSet::new();
        let mut more = false;
        for target in targets {
            if let Some(node) = self.runtime.graph().node(target) {
                for binding in node.payload().environments() {
                    let summary = binding_description(binding, true);
                    if summaries.contains(&summary) {
                        continue;
                    }
                    if summaries.len() < 4 {
                        summaries.insert(summary);
                    } else {
                        more = true;
                    }
                }
            }
        }
        let mut summary = summaries.into_iter().collect::<Vec<_>>().join("; ");
        if more {
            summary.push_str("; more bindings available in inspect");
        }
        summary
    }
}

fn short_revision(binding: &Binding) -> String {
    binding
        .environment()
        .revision()
        .to_string()
        .trim_start_matches("sha256:")
        .chars()
        .take(8)
        .collect()
}

fn binding_description(binding: &Binding, compact: bool) -> String {
    format!(
        "{} / {} · {} · rev {}",
        binding.environment().name(),
        binding.alias(),
        destination(binding),
        if compact {
            short_revision(binding)
        } else {
            binding.environment().revision().to_string()
        }
    )
}

fn destination(binding: &Binding) -> String {
    let target = format!("target {}", binding.import().target().name());
    binding
        .import()
        .endpoint()
        .and_then(|value| url::Url::parse(value).ok())
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .map(|url| {
            format!(
                "{} · {target}",
                url.origin()
                    .ascii_serialization()
                    .chars()
                    .take(512)
                    .collect::<String>()
            )
        })
        .unwrap_or(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        driver::CancellationToken,
        imports::ImportProduct,
        providers::{Call, InvocationFuture},
    };
    use wes_core::environments::{CapturedSource, CapturedSources, Package};
    use wes_core::{
        Primitive, Shape,
        capability::{Capability, ProviderDescription, Safety},
    };
    struct Never;
    impl Invoker for Never {
        fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
            panic!("admission must not invoke a provider")
        }
    }
    fn publish(workspace: &mut Workspace, endpoint: &str, extra: bool) {
        let other = if extra {
            ", extra: {source: {kind: spec, file: api.json}, bind: {target: local}}"
        } else {
            ""
        };
        let package = Package::parse(&format!("version: 1\ntargets: {{local: {{kind: local}}}}\nenvironments: {{dev: {{imports: {{api: {{source: {{kind: spec, file: api.json}}, bind: {{target: local, endpoint: '{endpoint}'}}}}{other}}}}}}}")).unwrap();
        let mut sources = CapturedSources::default();
        for key in package.required_sources() {
            sources
                .insert(
                    key,
                    CapturedSource::new("fixture/v1", "synthetic descriptor").unwrap(),
                )
                .unwrap();
        }
        let plan = workspace.environments.plan(&package, &sources).unwrap();
        workspace.environments.apply(plan).unwrap();
    }
    #[test]
    fn target_display_rename_does_not_change_execution_but_driver_config_does() {
        let mut workspace = fixture();
        let id = commit(&mut workspace, "api read > original");
        let captured = workspace
            .runtime
            .graph()
            .node(&id)
            .unwrap()
            .payload()
            .environments()
            .next()
            .unwrap()
            .clone();
        let renamed = Package::parse("version: 1\ntargets: {local_1: {kind: local}}\nenvironments: {dev: {imports: {api: {source: {kind: spec, file: api.json}, bind: {target: local_1, endpoint: 'http://localhost:8771'}}}}}").unwrap();
        let mut sources = CapturedSources::default();
        for key in renamed.required_sources() {
            sources
                .insert(
                    key,
                    CapturedSource::new("fixture/v1", "synthetic descriptor").unwrap(),
                )
                .unwrap();
        }
        let plan = workspace.environments.plan(&renamed, &sources).unwrap();
        workspace.environments.apply(plan).unwrap();
        assert!(workspace.binding_current(&captured));
        let changed = Package::parse("version: 1\ntargets: {local_1: {kind: local, env: {MODE: changed}}}\nenvironments: {dev: {imports: {api: {source: {kind: spec, file: api.json}, bind: {target: local_1, endpoint: 'http://localhost:8771'}}}}}").unwrap();
        let plan = workspace.environments.plan(&changed, &sources).unwrap();
        workspace.environments.apply(plan).unwrap();
        assert!(!workspace.binding_current(&captured));
    }
    fn fixture() -> Workspace {
        let mut workspace = Workspace::new();
        publish(&mut workspace, "http://localhost:8771", false);
        let binding = workspace
            .environments
            .inspect("dev")
            .unwrap()
            .bind("api")
            .unwrap();
        let product = ImportProduct::new(
            ProviderDescription::new(
                "api",
                [Capability::new(
                    ["read"],
                    Shape::Primitive(Primitive::Text),
                    Safety::Safe,
                )],
                vec![],
            )
            .unwrap(),
            Arc::new(Never),
            vec![],
        )
        .unwrap();
        workspace
            .providers
            .register_environment(&product, binding, None)
            .unwrap();
        workspace
    }
    fn commit(workspace: &mut Workspace, source: &str) -> NodeId {
        let parsed = wes_language::parse(&wes_language::SourceText::new("fixture", source));
        let Preparation::Change(change) = workspace.prepare(&parsed.script.statements[0]).unwrap()
        else {
            panic!("declaration")
        };
        let id = change.node().unwrap().clone();
        workspace.commit(change).unwrap();
        id
    }
    #[test]
    fn unrelated_publication_preserves_binding_but_changed_endpoint_blocks_calls_and_calc() {
        let mut workspace = fixture();
        let direct = commit(&mut workspace, "api read > original");
        let calc = commit(
            &mut workspace,
            ":calc { return call('api', ['read'], {}); } > calculated",
        );
        let before = workspace.environments.inspect("dev").unwrap().revision();
        publish(&mut workspace, "http://localhost:8771", true);
        assert_ne!(
            before,
            workspace.environments.inspect("dev").unwrap().revision()
        );
        for id in [&direct, &calc] {
            assert!(
                workspace
                    .check_task_bindings(workspace.runtime.graph().node(id).unwrap().payload())
                    .is_ok()
            );
        }
        publish(&mut workspace, "http://localhost:8772", true);
        for id in [&direct, &calc] {
            let error = workspace
                .check_task_bindings(workspace.runtime.graph().node(id).unwrap().payload())
                .unwrap_err();
            assert_eq!(error.code, "ENV039");
            assert!(error.message.contains("8771"));
            assert!(error.message.contains("New branch"));
        }
    }
    #[test]
    fn queued_ticket_is_rechecked_and_group_refusal_does_not_change_runs() {
        let mut workspace = fixture();
        let id = commit(&mut workspace, "api read > original");
        let ticket = workspace
            .start(Duration::ZERO)
            .into_iter()
            .find_map(|effect| match effect {
                Effect::Spawn(ticket) => Some(ticket),
                _ => None,
            })
            .unwrap();
        publish(&mut workspace, "http://localhost:8772", false);
        let before = workspace.runtime.run_of(&id).cloned();
        assert!(
            workspace
                .repeat_nodes(&[id.clone()], Duration::ZERO)
                .is_err()
        );
        assert_eq!(before.as_ref(), workspace.runtime.run_of(&id));
        assert_eq!(workspace.enter_ticket(ticket).unwrap_err().code(), "ENV039");
    }
    #[test]
    fn refresh_summaries_deduplicate_bindings_and_use_short_revisions() {
        let mut workspace = fixture();
        let first = commit(&mut workspace, "api read > original");
        let second = commit(
            &mut workspace,
            ":calc { return call('api', ['read'], {}); } > another",
        );
        let one = workspace.refresh_binding_summary(std::slice::from_ref(&first));
        let group = workspace.refresh_binding_summary(&[first, second]);
        assert_eq!(one, group);
        assert!(one.contains("http://localhost:8771 · target local"));
        assert!(!one.contains("sha256:"));
        assert_eq!(one.split(" · rev ").last().unwrap().len(), 8);
    }
    #[test]
    fn captured_destination_projection_never_exposes_url_material() {
        let mut workspace = fixture();
        publish(
            &mut workspace,
            "https://user:secret@example.invalid:9443/private-token?key=value#fragment",
            false,
        );
        let binding = workspace
            .environments
            .inspect("dev")
            .unwrap()
            .bind("api")
            .unwrap();
        assert_eq!(
            destination(&binding),
            "https://example.invalid:9443 · target local"
        );
    }
}
