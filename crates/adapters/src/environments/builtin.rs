//! Built-in capabilities use the same captured binding and authority as imported contracts.
use super::*;

pub(super) fn build(
    alias: &str,
    binding: &Binding,
    authority: Authority,
    docker_candidates: &[std::path::PathBuf],
) -> Result<ImportProduct, EnvironmentError> {
    let import = binding.import();
    let target = import.target();
    driver(target).validate(target).map_err(invalid)?;
    let name = import.source().bytes();
    if name == "sh" && !driver(target).local_provider_io() {
        return driver(target)
            .build_process(alias, binding, authority)
            .map_err(invalid);
    }
    if name != "http" && !driver(target).local_provider_io() {
        return Err(invalid(&format!(
            "Built-in '{name}' currently requires a local target; its remote provider transport is not implemented. Remote sh/process execution is supported.",
        )));
    }
    if name == "docker" {
        if !import.credential_refs().is_empty()
            || target.cwd().is_some()
            || !target.variables().is_empty()
        {
            return Err(invalid(
                "Docker does not accept process settings or credential bindings",
            ));
        }
        if import.declaration().binding_mode == wes_core::environments::BindingMode::Automatic {
            return crate::docker::automatic::build(alias, binding, authority, docker_candidates)
                .map_err(invalid);
        }
        return match import.endpoint() {
            None => crate::docker::unconnected(alias).map_err(|e| invalid(&e.to_string())),
            Some(endpoint) => {
                let socket = endpoint.strip_prefix("unix://")
                    .filter(|path| path.starts_with('/'))
                    .ok_or_else(|| invalid("Docker endpoint requires unix:///absolute/path/to/docker.sock; use bind: auto for local discovery; remote transports are not implemented"))?;
                crate::docker::observation::build_at(alias, socket, binding, authority)
                    .map_err(invalid)
            }
        };
    }
    if import.endpoint().is_some() {
        return Err(invalid(
            "The generic sh/http providers do not bind a service endpoint; http request takes url per call",
        ));
    }
    match name {
        "sh" => {
            let mut config = ProcessConfig::default();
            if let Some(ms) = import.timeout_ms() {
                config.timeout = Duration::from_millis(ms as u64);
            }
            let (description, invoker) = crate::process::shell_named(alias, config)
                .map_err(|_| invalid("Invalid shell provider configuration"))?;
            let credentials = authority
                .credentials(binding, alias)
                .map_err(|_| invalid("Invalid scoped shell credentials"))?;
            let invoker = invoker
                .managed(
                    target,
                    credentials,
                    import.credential_refs().keys().cloned().collect(),
                )
                .map_err(|_| invalid("Invalid shell target settings"))?;
            let conversation = Arc::new(invoker.piped());
            ImportProduct::new(description, Arc::new(invoker), vec![])
                .map(|product| product.with_conversations(conversation))
                .map_err(|_| invalid("Shell metadata exceeds budget"))
        }
        "http" => {
            if !import.credential_refs().is_empty() {
                return Err(invalid(
                    "Generic HTTP cannot bind credentials to arbitrary call URLs; use a service provider with declared authentication",
                ));
            }
            let mut config = HttpConfig::default();
            if let Some(ms) = import.timeout_ms() {
                config.request_timeout = Duration::from_millis(ms as u64);
            }
            let (description, invoker) = crate::http::direct_named(alias, config)
                .map_err(|_| invalid("Invalid HTTP provider configuration"))?;
            let transport =
                crate::http::transport::Transport::bound(binding, authority).map_err(invalid)?;
            ImportProduct::new(
                description,
                Arc::new(invoker.with_transport(transport)),
                vec![],
            )
            .map_err(|_| invalid("HTTP metadata exceeds budget"))
        }
        _ => Err(invalid("Unknown built-in provider")),
    }
}
