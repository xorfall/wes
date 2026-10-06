pub(crate) mod command;
pub(crate) mod finite;
mod terminal;
pub use terminal::{HostLaunch, TerminalPlan, TerminalSupport};
// Compiled target dispatch. Environment wiring uses namespaces and supported operations,
// never a growing list of remote-vendor exceptions. No target construction performs I/O.
use std::{path::Path, sync::Arc, time::Duration};
use wes_core::environments::{Binding, Target, TargetKind};
use wes_engine::{environments::Authority, imports::ImportProduct};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessNamespace {
    Host,
    Target,
}

pub(crate) trait TargetDriver: Sync {
    fn namespace(&self) -> ProcessNamespace;
    fn terminal(&self, _target: &Target) -> Result<TerminalPlan, &'static str> {
        Err("This execution target does not support terminal sessions")
    }
    fn local_provider_io(&self) -> bool {
        false
    }
    fn validate(&self, target: &Target) -> Result<(), &'static str>;
    /// Fresh planning only; never called when rebuilding saved bindings.
    fn validate_inputs(&self, _target: &Target) -> Result<(), &'static str> {
        Ok(())
    }
    fn build_process(
        &self,
        alias: &str,
        binding: &Binding,
        authority: Authority,
    ) -> Result<ImportProduct, &'static str>;
}

struct Local;
struct Docker;
struct Ssh;

pub(crate) fn driver(target: &Target) -> &'static dyn TargetDriver {
    match target.kind() {
        TargetKind::Local => &Local,
        TargetKind::Docker { .. } => &Docker,
        TargetKind::Ssh(_) => &Ssh,
    }
}

impl TargetDriver for Local {
    fn terminal(&self, target: &Target) -> Result<TerminalPlan, &'static str> {
        self.validate(target)?;
        if !cfg!(any(unix, windows)) {
            return Err("Terminal transport is not supported on this host platform");
        }
        if target
            .variables()
            .keys()
            .any(|key| terminal::reserved_variable(key))
        {
            return Err("Target variables cannot replace terminal integration/startup controls");
        }
        Ok(terminal::local(Some(target.clone())))
    }
    fn namespace(&self) -> ProcessNamespace {
        ProcessNamespace::Host
    }
    fn local_provider_io(&self) -> bool {
        true
    }
    fn validate(&self, target: &Target) -> Result<(), &'static str> {
        // Only a declared directory is checked here; the default is resolved when a launch
        // is bound, by the same rule.
        if let Some(cwd) = target.cwd() {
            crate::process::local_directory(Some(cwd))?;
        }
        Ok(())
    }
    fn build_process(
        &self,
        alias: &str,
        binding: &Binding,
        authority: Authority,
    ) -> Result<ImportProduct, &'static str> {
        let import = binding.import();
        self.validate(import.target())?;
        if import.source().format() != "process/path/v1"
            || !Path::new(import.source().bytes()).is_absolute()
            || import.endpoint().is_some()
        {
            return Err(
                "Local process bindings require a captured host executable and no endpoint override",
            );
        }
        let credentials = authority
            .credentials(binding, alias)
            .map_err(|_| "Invalid scoped credential binding")?;
        let mut config = crate::process::ProcessConfig::default();
        if let Some(ms) = import.timeout_ms() {
            config.timeout = Duration::from_millis(ms as u64);
        }
        let (description, invoker) =
            crate::process::wrapping(alias, import.source().bytes(), config)
                .map_err(|_| "Invalid captured process binding")?;
        let invoker = invoker
            .managed(
                import.target(),
                credentials,
                import.credential_refs().keys().cloned().collect(),
            )
            .map_err(|_| "Unsupported managed process launch configuration")?;
        let conversation = Arc::new(invoker.piped());
        ImportProduct::new(description, Arc::new(invoker), vec![
            "Managed process environment is explicit; native executables may still consult ambient files or external services.".into()
        ]).map(|product| product.with_conversations(conversation))
            .map_err(|_| "Process environment metadata exceeds budget")
    }
}

impl TargetDriver for Docker {
    fn terminal(&self, target: &Target) -> Result<TerminalPlan, &'static str> {
        self.validate(target)?;
        crate::docker::terminal(target)
    }
    fn namespace(&self) -> ProcessNamespace {
        ProcessNamespace::Target
    }
    fn validate(&self, target: &Target) -> Result<(), &'static str> {
        if target.cwd().is_some_and(|cwd| !cwd.starts_with('/')) {
            return Err("Docker cwd must be an absolute container path");
        }
        Ok(())
    }
    fn build_process(
        &self,
        alias: &str,
        binding: &Binding,
        authority: Authority,
    ) -> Result<ImportProduct, &'static str> {
        self.validate(binding.import().target())?;
        crate::docker::build(alias, binding, authority)
    }
}

impl TargetDriver for Ssh {
    fn validate_inputs(&self, target: &Target) -> Result<(), &'static str> {
        let TargetKind::Ssh(config) = target.kind() else {
            return Err("Expected an SSH target");
        };
        crate::ssh::validate_inputs(config)
    }
    fn terminal(&self, target: &Target) -> Result<TerminalPlan, &'static str> {
        self.validate(target)?;
        if !cfg!(any(unix, windows)) {
            return Err("Terminal transport is not supported on this host platform");
        }
        crate::ssh::terminal(target)
    }
    fn namespace(&self) -> ProcessNamespace {
        ProcessNamespace::Target
    }
    fn validate(&self, target: &Target) -> Result<(), &'static str> {
        crate::ssh::validate(target)
    }
    fn build_process(
        &self,
        alias: &str,
        binding: &Binding,
        authority: Authority,
    ) -> Result<ImportProduct, &'static str> {
        crate::ssh::build(alias, binding, authority)
    }
}

/// Pure capability discovery; no file, process, credential or network probes.
pub fn terminal_support(target: &Target) -> Result<TerminalSupport, &'static str> {
    driver(target).terminal(target).map(|plan| plan.support())
}
