//! A finite POSIX command contract, independent of Docker/SSH wire encoding.
//! Remote argv may be retained by a daemon/server; do not export confidential arguments.
use std::time::Duration;
use wes_core::{
    Data, Primitive, Shape,
    capability::{Capability, Parameter, Safety},
    environments::Binding,
};
use wes_engine::providers::Call;

pub(crate) struct Program {
    argv: Vec<String>,
    argument: &'static str,
}
impl Program {
    pub(crate) fn capture(binding: &Binding) -> Result<Self, &'static str> {
        let source = binding.import().source();
        let (argv, argument) = match (source.format(), source.bytes()) {
            ("builtin/v1", "sh") => (vec!["/bin/sh".into(), "-c".into()], "cmd"),
            ("process/target/v1", path)
                if path.starts_with('/') && !path.contains('\0') && path.len() <= 4096 =>
            {
                (vec![path.into()], "args")
            }
            _ => {
                return Err(
                    "Remote execution requires a target executable or the built-in POSIX sh provider",
                );
            }
        };
        Ok(Self { argv, argument })
    }
    pub(crate) fn capability(&self) -> Capability {
        let mut run = Capability::new(["run"], crate::process::output_shape(), Safety::Unsafe);
        run.summary =
            "Finite execution on the bound target; cancellation may leave remote work running"
                .into();
        run.parameters = vec![
            Parameter::new(
                self.argument,
                Shape::Primitive(Primitive::Text),
                self.argument == "cmd",
            ),
            Parameter::new("timeout", Shape::Primitive(Primitive::Duration), false),
        ];
        if self.argument == "cmd" {
            run.parameters[0].content = Some("sh".into());
        }
        run
    }
    pub(crate) fn prepare(
        &self,
        binding: &Binding,
        call: &Call,
    ) -> Result<(Vec<String>, Duration), &'static str> {
        if call.arguments.values().any(|v| {
            v.provenance().policy().is_confidential() || v.provenance().policy().is_unknown()
        }) {
            return Err(
                "Remote exec does not support confidential arguments or unknown-policy arguments; argv can be retained by the remote service",
            );
        }
        let mut argv = self.argv.clone();
        if let Some(value) = call.arguments.get(self.argument) {
            let Data::Text(text) = value.data() else {
                return Err("Remote command requires a Text argument");
            };
            if text.contains('\0') || text.len() > 1024 * 1024 {
                return Err("Remote argument exceeds 1 MiB or contains NUL");
            }
            argv.push(text.to_string());
        } else if self.argument == "cmd" {
            return Err("sh run requires cmd: with a POSIX shell command");
        }
        if argv
            .iter()
            .try_fold(0usize, |size, arg| size.checked_add(arg.len() + 1))
            .is_none_or(|size| size > 1024 * 1024)
        {
            return Err("Remote command exceeds 1 MiB");
        }
        let timeout = match call.arguments.get("timeout") {
            None => Duration::from_millis(binding.import().timeout_ms().unwrap_or(300_000) as u64),
            Some(value) => {
                let Data::Duration(duration) = value.data() else {
                    return Err("timeout requires Duration");
                };
                let parts = duration.parts();
                Duration::new(
                    u64::try_from(parts.seconds()).map_err(|_| "timeout must be positive")?,
                    parts.nanos(),
                )
            }
        };
        if timeout.is_zero() || timeout > Duration::from_secs(3600) {
            return Err("Remote timeout must be positive and at most one hour");
        }
        Ok((argv, timeout))
    }
}
