use super::*;
use crate::codec::{self, Limits};
use indexmap::IndexMap;
use wes_core::{Data, ErrorId, ErrorValue, Provenance, Shape, Value, contracts::boundary};
use wes_engine::{
    driver::CancellationToken,
    imports::{ImportError, ImportProduct},
    providers::{Call, InvocationError, InvocationFuture, Invoker},
};

#[derive(Clone)]
struct MappedInvoker {
    definition: Arc<Definition>,
    base: Arc<dyn Invoker>,
    capability: Arc<wes_core::capability::Capability>,
    config: crate::process::ProcessConfig,
}
pub(super) fn wrap(
    definition: Definition,
    base: &ProviderDescription,
    invoker: Arc<dyn Invoker>,
    config: crate::process::ProcessConfig,
    warnings: Vec<String>,
) -> Result<ImportProduct, ImportError> {
    let capability = base
        .capability(&["run".into()])
        .ok_or(ImportError::InvalidRecipe)?
        .clone();
    ImportProduct::new(
        definition.description.clone(),
        Arc::new(MappedInvoker {
            definition: Arc::new(definition),
            base: invoker,
            capability,
            config,
        }),
        warnings,
    )
}
pub(crate) fn build(
    alias: &str,
    binding: &wes_core::environments::Binding,
    authority: wes_engine::environments::Authority,
) -> Result<ImportProduct, ImportError> {
    let driver = crate::execution_targets::driver(binding.import().target());
    let remote = driver.namespace() == crate::execution_targets::ProcessNamespace::Target;
    let (definition, _) =
        super::importer::reconstruct(binding.import().source().bytes(), remote, Some(alias))?;
    if definition.session.is_some() {
        return super::session::bound(definition, alias, binding, authority);
    }
    if binding.import().endpoint().is_some() {
        return Err(ImportError::Input(
            "Finite process providers cannot bind an endpoint; use a session backend",
        ));
    }
    let base = driver
        .build_process(alias, binding, authority)
        .map_err(ImportError::Input)?;
    wrap(
        definition,
        base.description(),
        base.invoker().clone(),
        crate::process::ProcessConfig::default(),
        base.warnings().iter().map(|w| w.message.clone()).collect(),
    )
}

fn failed(code: &str, message: impl Into<String>) -> InvocationError {
    InvocationError::Failed(
        ErrorValue::new(
            ErrorId::new(uuid::Uuid::new_v4().to_string()).expect("uuid"),
            code,
            message,
            vec![],
            None,
        )
        .expect("fixed error"),
    )
}
fn scalar(data: &Data, limit: usize) -> Result<String, InvocationError> {
    if matches!(data,Data::Text(s) if s.len()>limit) {
        return Err(failed("PRV001", "scalar argv value exceeds its byte limit"));
    }
    Ok(match data {
        Data::Text(s) => s.to_string(),
        Data::Int(n) => n.to_string(),
        Data::Decimal(n) => n.to_string(),
        Data::Bool(v) => v.to_string(),
        Data::Instant(v) => v.to_string(),
        Data::Duration(v) => v.to_string(),
        Data::Interval(v) => v.to_string(),
        _ => return Err(failed("PRV001", "argv encoding requires a scalar value")),
    })
}
impl MappedInvoker {
    fn arguments(
        &self,
        op: &Operation,
        call: &Call,
        token: &CancellationToken,
    ) -> Result<IndexMap<String, Value>, InvocationError> {
        if call
            .arguments
            .keys()
            .any(|name| !op.parameters.iter().any(|(n, _, _)| n == name))
        {
            return Err(failed("PRV001", "undeclared provider argument"));
        }
        for (name, contract, required) in &op.parameters {
            match call.arguments.get(name) {
                None if *required => {
                    return Err(failed(
                        "PRV001",
                        format!("required provider argument {name} is missing"),
                    ));
                }
                Some(value) => {
                    boundary::require(
                        name,
                        std::slice::from_ref(contract),
                        &contract.shape(),
                        value,
                        &|| token.is_cancelled(),
                    )
                    .map_err(|e| match e {
                        boundary::BoundaryError::Cancelled(_) => InvocationError::Cancelled,
                        _ => failed(
                            "PRV001",
                            format!("provider argument {name} violates its captured contract"),
                        ),
                    })?;
                }
                _ => {}
            }
        }
        let mut argv = Vec::new();
        let mut bytes = self.definition.executable.len() + 1;
        // Charge every emitted element before appending it. List expansion cannot bypass
        // the shared process count/byte bounds, even if prefixes repeat.
        let mut push = |s: String| -> Result<(), InvocationError> {
            bytes = bytes
                .checked_add(s.len() + 1)
                .ok_or_else(|| failed("PRV001", "argv byte limit exceeded"))?;
            if s.contains('\0')
                || bytes > self.config.argument_bytes
                || argv.len() + 1 >= crate::process::MAX_ARGUMENTS
            {
                return Err(failed(
                    "PRV001",
                    "mapped argv exceeds its count/byte limit or contains NUL",
                ));
            }
            argv.push(Data::Text(s.into()));
            Ok(())
        };
        for arg in &op.argv {
            if token.is_cancelled() {
                return Err(InvocationError::Cancelled);
            }
            match arg {
                schema::Argument::Literal(s) => push(s.clone())?,
                schema::Argument::Parameter(schema::ArgumentFields {
                    parameter,
                    prefix,
                    encoding,
                }) => {
                    let Some(value) = call.arguments.get(parameter) else {
                        continue;
                    };
                    let data = match value.data() {
                        Data::Option(_) if matches!(encoding, schema::Encoding::Json) => {
                            value.data()
                        }
                        Data::Option(None) => continue,
                        Data::Option(Some(v)) => v.as_ref(),
                        data => data,
                    };
                    let parts = match encoding {
                        schema::Encoding::Scalar => vec![scalar(data, self.config.argument_bytes)?],
                        schema::Encoding::Spread => {
                            let Data::List(values) = data else {
                                return Err(failed("PRV001", "spread encoding requires a list"));
                            };
                            // Bound allocation before collecting or cloning a supplied list.
                            if values.len() >= crate::process::MAX_ARGUMENTS {
                                return Err(failed("PRV001", "spread encoding exceeds argv limit"));
                            }
                            let mut parts = Vec::new();
                            let mut total = 0usize;
                            for value in values {
                                let part = scalar(value, self.config.argument_bytes)?;
                                total = total.saturating_add(part.len() + 1);
                                if total > self.config.argument_bytes {
                                    return Err(failed(
                                        "PRV001",
                                        "spread argv exceeds its byte limit",
                                    ));
                                }
                                parts.push(part);
                            }
                            parts
                        }
                        schema::Encoding::Flag => match data {
                            Data::Bool(false) => continue,
                            Data::Bool(true) => vec![String::new()],
                            _ => return Err(failed("PRV001", "flag encoding requires Bool")),
                        },
                        schema::Encoding::Json => vec![
                            String::from_utf8(
                                codec::encode_request_data(
                                    data,
                                    Limits {
                                        bytes: self.config.argument_bytes,
                                        nodes: 100_000,
                                    },
                                )
                                .map_err(|_| {
                                    failed("PRV001", "JSON argument exceeds its encoding limits")
                                })?,
                            )
                            .expect("JSON is UTF-8"),
                        ],
                    };
                    for part in parts {
                        for prefix in prefix {
                            push(prefix.clone())?;
                        }
                        if !matches!(encoding, schema::Encoding::Flag) {
                            push(part)?;
                        }
                    }
                }
            }
        }
        let provenance = Provenance::agreed_by(call.arguments.values().map(Value::provenance));
        let value = Value::new(
            Shape::List(Box::new(Shape::Primitive(wes_core::Primitive::Text))),
            Data::List(argv),
            provenance,
        )
        .expect("literal argv");
        Ok(IndexMap::from([("args".into(), value)]))
    }
    fn output(
        &self,
        op: &Operation,
        output: Value,
        token: &CancellationToken,
    ) -> Result<Value, InvocationError> {
        if token.is_cancelled() {
            return Err(InvocationError::Cancelled);
        }
        let Data::Record(fields) = output.data() else {
            return Err(failed(
                "PRV002",
                "process backend returned an invalid output",
            ));
        };
        let Some(Data::Int(code)) = fields.get("exitCode") else {
            return Err(failed("PRV002", "process backend omitted exitCode"));
        };
        if !op.success_codes.contains(code) {
            let issues = if let Some(Data::Bytes(stderr)) = fields.get("stderr") {
                let excerpt =
                    String::from_utf8_lossy(&stderr[..stderr.len().min(2048)]).into_owned();
                if excerpt.is_empty() {
                    vec![]
                } else {
                    vec![wes_core::ValidationIssue {
                        path: "/process/stderr".into(),
                        code: "PROCESS_STDERR".into(),
                        message: if stderr.len() > 2048 {
                            format!("{excerpt}… [truncated]")
                        } else {
                            excerpt
                        },
                    }]
                }
            } else {
                vec![]
            };
            return Err(InvocationError::Failed(ErrorValue::new(
                ErrorId::new(uuid::Uuid::new_v4().to_string()).expect("uuid"),"PRV003",
                format!("provider process exited with code {code}; stdout was not decoded. No retry was performed."),
                issues,None).expect("fixed error").with_policy(output.provenance().policy())));
        }
        if matches!(op.format, schema::OutputFormat::Process) {
            return Ok(output);
        }
        let Some(Data::Bytes(stdout)) = fields.get("stdout") else {
            return Err(failed("PRV002", "process backend omitted stdout"));
        };
        let contract = op.result.as_ref().expect("compiled output contract");
        let data = match op.format {
            schema::OutputFormat::Json => codec::decode_json_for_contract(
                stdout,
                Limits {
                    bytes: self.config.output_bytes,
                    nodes: 100_000,
                },
                contract,
            )
            .map_err(|_| {
                failed(
                    "PRV004",
                    "process stdout is invalid JSON or exceeds decoding limits",
                )
            })?,
            schema::OutputFormat::Text => Data::Text(
                std::str::from_utf8(stdout)
                    .map_err(|_| failed("PRV004", "process stdout is not valid UTF-8"))?
                    .into(),
            ),
            schema::OutputFormat::Bytes => Data::Bytes(stdout.clone()),
            schema::OutputFormat::Process => unreachable!(),
        };
        let value = Value::new(Shape::Unknown, data, output.provenance().clone())
            .map_err(|_| failed("PRV004", "invalid process result"))?;
        boundary::checked_result(contract, &value, &|| token.is_cancelled()).map_err(|e| match e {
            boundary::BoundaryError::Cancelled(_) => InvocationError::Cancelled,
            _ => failed(
                "PRV004",
                "process stdout violates its captured output contract",
            ),
        })
    }
}
impl Invoker for MappedInvoker {
    fn invoke_acquired(
        &self,
        call: Call,
        token: CancellationToken,
        context: wes_engine::storage::AcquisitionContext,
    ) -> wes_engine::storage::AcquiredFuture {
        let this = self.clone();
        Box::pin(async move {
            let op = this
                .definition
                .operations
                .iter()
                .find(|op| op.path == call.capability.path)
                .ok_or_else(|| failed("PRV001", "unknown captured provider operation"))?;
            let eligible = match op.format {
                schema::OutputFormat::Process => true,
                schema::OutputFormat::Json => false,
                schema::OutputFormat::Text | schema::OutputFormat::Bytes => {
                    op.result.as_ref().is_some_and(|c| {
                        let limits = c.constraints();
                        matches!(c.kind(), wes_core::contracts::ContractKind::Scalar(_))
                            && limits.enumeration.is_empty()
                            && limits.patterns.is_empty()
                            && limits.min.is_none()
                            && limits.max.is_none()
                            && limits.min_length.is_none()
                            && limits.max_length.is_none()
                            && limits.min_items.is_none()
                            && limits.max_items.is_none()
                    })
                }
            };
            if !eligible {
                return this
                    .invoke(call, token)
                    .await
                    .map(wes_engine::storage::AcquiredOutput::Value);
            }
            let arguments = this.arguments(op, &call, &token)?;
            let delegated = Call {
                capability: this.capability.clone(),
                arguments,
                ..call
            };
            let mapped = this.clone();
            let captured = op.clone();
            let cancel = token.clone();
            let context = context.with_finalizer(Arc::new(move |value, leaves| {
                let output = mapped.output(&captured, value, &cancel)?;
                if matches!(captured.format, schema::OutputFormat::Process) {
                    Ok((output, leaves))
                } else {
                    Ok((
                        output,
                        vec![wes_engine::storage::LeafSelection {
                            port: "stdout".into(),
                            fields: vec![],
                            text: matches!(captured.format, schema::OutputFormat::Text),
                        }],
                    ))
                }
            }));
            match this
                .base
                .invoke_acquired(delegated, token.clone(), context)
                .await?
            {
                wes_engine::storage::AcquiredOutput::Value(value) => this
                    .output(op, value, &token)
                    .map(wes_engine::storage::AcquiredOutput::Value),
                stored @ wes_engine::storage::AcquiredOutput::Stored { .. } => Ok(stored),
            }
        })
    }

    fn invoke(&self, call: Call, token: CancellationToken) -> InvocationFuture {
        let this = self.clone();
        Box::pin(async move {
            let op = this
                .definition
                .operations
                .iter()
                .find(|op| op.path == call.capability.path)
                .ok_or_else(|| failed("PRV001", "unknown captured provider operation"))?;
            let arguments = this.arguments(op, &call, &token)?;
            let delegated = Call {
                capability: this.capability.clone(),
                arguments,
                ..call
            };
            // The same authority, run and cancellation token reach the existing execution
            // driver. Unknown remote outcomes propagate unchanged; no replay/retry here.
            let output = this.base.invoke(delegated, token.clone()).await?;
            let policy = output.provenance().policy().clone();
            this.output(op, output, &token)
                .map_err(|error| match error {
                    InvocationError::Failed(error) => {
                        InvocationError::Failed(error.with_policy(&policy))
                    }
                    error => error,
                })
        })
    }
}
