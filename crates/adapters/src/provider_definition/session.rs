//! Owned executable sessions. Protocol frames are distinct from retained native values.
//! A session is never restarted or reconstructed from a result, delivery tag or public ID.
//!
//! A declaration uses backend `{kind: session, bin: ..., credentials: [Slot]}`;
//! each operation has literal argv, JSON result/type, optional streaming and
//! control `{input: Contract, output: Contract, output_policy: private}`
//! (streaming only). The optional static output policy defaults to public and
//! only tightens control results; inherited source/action policies still join.
//! A peer cannot choose or downgrade this classification. It launches
//! only on a local target, with declared cwd/env and no inherited environment.
//! The first stdin frame carries captured run, fresh attempt nonce, typed
//! arguments, endpoint, contract digests and separately granted credentials.
//! Peers must complete their external handshake before Ready. Stream events use
//! contiguous sequences and one-event host credit; finite operations send one
//! Result and End. Controls correlate exact action IDs with confirmed typed
//! data, refused or unknown outcomes. The host never equates a write with success.
//! Every frame has a bounded big-endian u32 byte length and strict UTF-8 JSON.
//! Declared Bytes fields use canonical padded standard base64 in either direction;
//! this conversion requires their captured contract and never applies to Text/Unknown.
//! EOF, invalid state, deadline or cancellation closes and joins the process
//! group/Job Object and both readers; no restart, replay or reconnection occurs.
mod protocol;
#[cfg(test)]
mod tests;
use super::*;
use crate::{codec, process::ProcessConfig};
use std::{collections::BTreeMap, path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    process::Command,
    sync::{mpsc, oneshot},
};
use wes_core::{Data, Provenance, Shape, Value, contracts::boundary};
use wes_engine::{
    credentials::{Credentials, ExposeSecret},
    driver::CancellationToken,
    environments::{Authority, InvocationAuthority},
    imports::{ImportError, ImportProduct},
    providers::{Call, InvocationError, InvocationFuture, Invoker},
    streams::{
        ControlFuture, ControlLimits, ControlOutcome, LiveController, StreamFuture, StreamSink,
        StreamingInvoker,
    },
};
use zeroize::Zeroizing;

#[derive(Clone)]
struct Session {
    definition: Arc<Definition>,
    config: ProcessConfig,
    cwd: PathBuf,
    variables: BTreeMap<String, String>,
    endpoint: Option<String>,
    credentials: Arc<dyn Credentials>,
    authority: Option<(Authority, wes_core::environments::Binding, String)>,
}
type CredentialSnapshot = BTreeMap<String, Option<wes_engine::credentials::Secret>>;
impl Session {
    fn credentials_current(&self, captured: &CredentialSnapshot) -> bool {
        self.credentials
            .snapshot(self.definition.description.secrets())
            .is_ok_and(|current| {
                current.len()==captured.len() && captured.iter().all(|(name,secret)| {
                matches!((secret,current.get(name)),(Some(a),Some(Some(b))) if Arc::ptr_eq(a,b))
            })
            })
    }
    fn dispatch_current(
        &self,
        policy: &wes_core::flow::FlowPolicy,
        captured: &CredentialSnapshot,
    ) -> bool {
        self.credentials_current(captured)
            && self
                .authority
                .as_ref()
                .is_none_or(|(authority, binding, _)| {
                    authority
                        .require_dispatch(
                            binding.environment().identity(),
                            policy,
                            binding.environment().revision(),
                        )
                        .is_ok()
                })
    }
}
fn error(message: &'static str) -> InvocationError {
    InvocationError::Failed(wes_engine::runtime::RuntimeCode::ExecutionFailed.error(message, None))
}
fn unknown() -> InvocationError {
    InvocationError::Failed(wes_engine::runtime::RuntimeCode::ExternalOutcomeUnknown.error(
        "Session completion could not be confirmed; the external outcome is unknown. No automatic retry was performed.",
        None,
    ))
}
fn product(session: Session) -> Result<ImportProduct, ImportError> {
    let description = session.definition.description.clone();
    let read_domain = session.definition.read_domain.clone();
    let session = Arc::new(session);
    let product = ImportProduct::new(description, session.clone(), vec![])?.with_streams(session);
    match read_domain {
        Some(domain) => product.with_read_domain(domain),
        None => Ok(product),
    }
}
pub(super) fn unbound(
    definition: Definition,
    config: ProcessConfig,
) -> Result<ImportProduct, ImportError> {
    product(Session {
        definition: Arc::new(definition),
        config,
        cwd: crate::process::local_directory(None).map_err(ImportError::Input)?,
        variables: BTreeMap::new(),
        endpoint: None,
        credentials: Arc::new(crate::credentials::MemoryCredentials::with_environment(
            Default::default(),
            |_| Ok(None),
        )),
        authority: None,
    })
}
pub(super) fn bound(
    definition: Definition,
    alias: &str,
    binding: &wes_core::environments::Binding,
    authority: Authority,
) -> Result<ImportProduct, ImportError> {
    if !matches!(
        binding.import().target().kind(),
        wes_core::environments::TargetKind::Local
    ) {
        return Err(ImportError::Input(
            "Session providers require a local owned duplex executable target",
        ));
    }
    if definition
        .description
        .secrets()
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        != binding.import().credential_refs().keys().collect()
    {
        return Err(ImportError::Input(
            "Session credential slots must exactly match the captured provider declaration",
        ));
    }
    let credentials = authority
        .credentials(binding, alias)
        .map_err(|_| ImportError::InvalidRecipe)?;
    let mut config = ProcessConfig::default();
    if let Some(ms) = binding.import().timeout_ms() {
        config.timeout = Duration::from_millis(ms as u64);
    }
    product(Session {
        definition: Arc::new(definition),
        config,
        cwd: crate::process::local_directory(binding.import().target().cwd())
            .map_err(ImportError::Input)?,
        variables: binding.import().target().variables().clone(),
        endpoint: binding.import().endpoint().map(str::to_owned),
        credentials,
        authority: Some((authority, binding.clone(), alias.into())),
    })
}
impl Session {
    fn permits(&self, caller: &InvocationAuthority, action: &Value) -> bool {
        // External control is a native user effect. A read/acquisition grant alone
        // never upgrades a cooperative actor into a write principal.
        caller.is_local_user()
            && self
                .authority
                .as_ref()
                .is_some_and(|(authority, binding, alias)| {
                    authority
                        .require_dispatch(
                            binding.environment().identity(),
                            action.provenance().policy(),
                            binding.environment().revision(),
                        )
                        .is_ok()
                        && authority.status(binding, alias).is_ok_and(|s| {
                            s.enabled
                                && (self.definition.description.secrets().is_empty()
                                    || s.access.granted())
                        })
                })
    }
    fn prepare(
        &self,
        call: &Call,
        operation: &Operation,
        token: &CancellationToken,
        session: &str,
    ) -> Result<(Zeroizing<Vec<u8>>, CredentialSnapshot), InvocationError> {
        if call
            .arguments
            .keys()
            .any(|k| !operation.parameters.iter().any(|(n, _, _)| n == k))
        {
            return Err(error(
                "Undeclared session argument; no executable was launched",
            ));
        }
        for (name, contract, required) in &operation.parameters {
            match call.arguments.get(name) {
                None if *required => {
                    return Err(error(
                        "Required session argument is missing; no executable was launched",
                    ));
                }
                Some(value) => {
                    boundary::require(name, std::slice::from_ref(contract), &contract.shape(), value, &|| token.is_cancelled()).map_err(|_| error("Session argument violates its captured contract; no executable was launched"))?;
                }
                _ => {}
            }
        }
        let data = Data::Record(
            call.arguments
                .iter()
                .map(|(n, v)| (n.clone(), v.data().clone()))
                .collect(),
        );
        let arguments = codec::encode_request_data(&data, protocol::limits())
            .map_err(|_| error("Session arguments exceed the wire budget"))?;
        let arguments = serde_json::value::RawValue::from_string(
            String::from_utf8(arguments).expect("JSON UTF-8"),
        )
        .map_err(|_| error("Invalid session arguments"))?;
        let names = self.definition.description.secrets();
        let secrets = self.credentials.snapshot(names).map_err(|_| {
            error("Session credential access is unavailable; no executable was launched")
        })?;
        if secrets.len() != names.len()
            || names
                .iter()
                .any(|n| !secrets.get(n).is_some_and(Option::is_some))
        {
            return Err(error(
                "Required session credential is missing; no executable was launched",
            ));
        }
        let credential_bytes = secrets.values().flatten().try_fold(0usize, |size, value| {
            size.checked_add(value.expose_secret().len())
        });
        if !credential_bytes.is_some_and(|n| n <= protocol::max_frame()) {
            return Err(error("Session credentials exceed the handoff budget"));
        }
        let start = protocol::Start {
            version: 1,
            session,
            run: call.run.id().as_str(),
            operation: &operation.path,
            arguments: &arguments,
            endpoint: self.endpoint.as_deref(),
            credentials: protocol::Secrets(&secrets),
            contracts: protocol::Contracts {
                result: operation.result.as_ref().expect("session result").digest(),
                parameters: operation
                    .parameters
                    .iter()
                    .map(|(name, c, _)| (name.as_str(), c.digest()))
                    .collect(),
                control: operation
                    .control
                    .as_ref()
                    .map(|control| [control.input.digest(), control.output.digest()]),
            },
            event_credit: u8::from(operation.streaming),
        };
        let encoded = Zeroizing::new(
            serde_json::to_vec(&start).map_err(|_| error("Session start encoding failed"))?,
        );
        if encoded.len() > protocol::max_frame() {
            return Err(error("Session start exceeds the frame budget"));
        }
        Ok((encoded, secrets))
    }
    async fn execute(
        self,
        call: Call,
        sink: Option<StreamSink>,
        token: CancellationToken,
    ) -> Result<Option<Value>, InvocationError> {
        let operation = self
            .definition
            .operations
            .iter()
            .find(|o| o.path == call.capability.path && o.streaming == sink.is_some())
            .ok_or_else(|| error("Captured session operation is unavailable"))?
            .clone();
        let session = uuid::Uuid::new_v4().to_string();
        let (start, credentials) = self.prepare(&call, &operation, &token, &session)?;
        let availability = self.authority.as_ref().map(|(authority,binding,_)| {
            authority.availability_lease(binding.environment().identity())
                .map_err(|_|error("Session environment authority is unavailable; no executable was launched"))
        }).transpose()?.unwrap_or_default();
        if token.is_cancelled() {
            return Err(InvocationError::Cancelled);
        }
        let executable = self.definition.executable.clone();
        let argv: Vec<_> = operation
            .argv
            .iter()
            .map(|a| match a {
                super::schema::Argument::Literal(s) => s.clone(),
                _ => unreachable!("compiled session argv"),
            })
            .collect();
        if argv
            .iter()
            .try_fold(executable.len() + 1, |bytes, arg| {
                bytes.checked_add(arg.len()).and_then(|n| n.checked_add(1))
            })
            .is_none_or(|bytes| bytes > self.config.argument_bytes)
        {
            return Err(error(
                "Session launch arguments exceed their byte budget; no executable was launched",
            ));
        }
        let cwd = self.cwd.clone();
        let variables = self.variables.clone();
        let launch_authority = self.clone();
        let launch_credentials = credentials.clone();
        let carried = Provenance::agreed_by(call.arguments.values().map(Value::provenance));
        let launch_token = token.clone();
        let launch_availability = availability.clone();
        let mut child = tokio::select! {biased;
            ()=token.cancelled()=>return Err(InvocationError::Cancelled),
            child=crate::process::serialized_spawn_async(move || {
                if launch_token.is_cancelled() || launch_availability.is_cancelled() || !launch_authority.dispatch_current(carried.policy(),&launch_credentials) {
                    return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
                }
                let mut command = Command::new(executable);
                command.args(argv).env_clear().envs(variables).current_dir(cwd)
                    .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
                crate::process::ownership::LocalChild::spawn(command, false)
            })=>child.map_err(|_| error("Session executable could not be launched"))?,
        };
        let mut stdin = child.inner().stdin.take().expect("piped stdin");
        let stdout = child.inner().stdout.take().expect("piped stdout");
        let stderr = child.inner().stderr.take().expect("piped stderr");
        let io_stop = CancellationToken::new();
        let (frames, mut read) = mpsc::channel(1);
        let reader = protocol::reader(stdout, frames, io_stop.clone());
        let mut diagnostics = protocol::diagnostics(stderr, io_stop.clone());
        let mut diagnostics_done = false;
        let mut diagnostics_failed = false;
        let (controls, mut requests) = mpsc::channel(1);
        let mut pending: Option<(
            String,
            oneshot::Sender<ControlOutcome>,
            tokio::time::Instant,
        )> = None;
        let ready_until = tokio::time::Instant::now()
            + self
                .config
                .timeout
                .min(Duration::from_millis(
                    wes_budgets::get("provider.session.ready.ms") as u64,
                ));
        let finite_until = tokio::time::Instant::now() + self.config.timeout;
        let mut opened = false;
        let mut sequence = 0u64;
        let mut output = None;
        let mut ended = false;
        // Before a complete frame is handed off, failures are known locally. Once
        // a finite peer may have received it, only a validated result or explicit
        // failure settles its outcome. Readiness and cleanup are not receipts.
        let mut start_handed_off = false;
        let mut finite_confirmed = false;
        let mut control_unconfirmed = false;
        let mut delivery: Option<
            std::pin::Pin<
                Box<
                    dyn std::future::Future<Output = Result<(), wes_engine::streams::StreamError>>
                        + Send,
                >,
            >,
        > = None;
        let work = {
            let work = async {
                if token.is_cancelled() {
                    return Err(InvocationError::Cancelled);
                }
                protocol::write_until(
                    &mut stdin,
                    &start,
                    &token,
                    ready_until,
                    Some(&mut start_handed_off),
                )
                .await?;
                drop(start);
                loop {
                    let until = pending.as_ref().map(|(_, _, until)| *until).unwrap_or(
                        if opened && sink.is_some() {
                            tokio::time::Instant::now() + Duration::from_secs(86_400)
                        } else if opened {
                            finite_until
                        } else {
                            ready_until
                        },
                    );
                    tokio::select! {biased;
                        ()=token.cancelled()=>return Err(InvocationError::Cancelled),
                        _=tokio::time::sleep_until(until), if !(opened && sink.is_some() && pending.is_none())=>return Err(error("Session response deadline elapsed; no retry was performed")),
                        result=&mut diagnostics, if !diagnostics_done=>{
                            diagnostics_done=true;
                            if !matches!(result,Ok(Ok(()))) {diagnostics_failed=true; return Err(error("Session diagnostics exceeded their byte budget or failed"));}
                        },
                        result=async {delivery.as_mut().expect("guarded delivery").await}, if delivery.is_some()=>{
                            result.map_err(|_|error("Session event admission closed or exceeded its budget"))?;
                            delivery=None;
                            if ended {return Ok(());}
                            protocol::write_until(&mut stdin,&protocol::credit(&session),&token,tokio::time::Instant::now()+wes_engine::streams::MAX_READY_WAIT,None).await?;
                        },
                        request=requests.recv(), if pending.is_none() && operation.control.is_some() && opened && !ended=>{
                            let Some(Request{action, reply, cancellation})=request else {return Err(error("Session control owner ended"));};
                            if cancellation.is_cancelled() || !self.dispatch_current(action.provenance().policy(),&credentials) {let _=reply.send(ControlOutcome::Refused); continue;}
                            let id=uuid::Uuid::new_v4().to_string();
                            let encoded=match protocol::action(&session,&id,&action) {Ok(encoded)=>encoded, Err(error)=>{let _=reply.send(ControlOutcome::Refused);return Err(error);}};
                            let until=tokio::time::Instant::now()+wes_engine::streams::MAX_READY_WAIT;
                            pending=Some((id,reply,until));
                            protocol::write_until(&mut stdin,&encoded,&token,until,None).await?;
                        },
                        frame=read.recv(), if !ended=>{
                            let frame=frame.ok_or_else(||error("Session protocol ended without a terminal frame; no retry was performed"))??;
                            if frame.version!=1 || frame.session!=session {return Err(error("Session protocol version or attempt identity is invalid"));}
                            match frame.message {
                                protocol::Message::Ready if !opened => {
                                    if let Some(sink)=&sink {
                                        sink.opened().map_err(|_| error("Session source closed before readiness"))?;
                                        if let Some(control)=&operation.control {
                                            sink.install_controls(control.input.clone(),control.output.clone(),ControlLimits::default(),Arc::new(Controller{session:self.clone(), send:controls.clone(),credentials:credentials.clone()})).map_err(|_| error("Session control installation was refused"))?;
                                        }
                                    }
                                    opened=true;
                                },
                                protocol::Message::Event{sequence:next,data} if opened && sink.is_some() && delivery.is_none()=>{
                                    sequence=sequence.checked_add(1).ok_or_else(|| error("Session event sequence exhausted"))?;
                                    if sequence!=next {return Err(error("Session event sequence is not contiguous; coverage ended"));}
                                    let value=protocol::value(data.get().as_bytes(),operation.result.as_ref().expect("result"),&token)?;
                                    let sink=sink.as_ref().expect("stream").clone();
                                    delivery=Some(Box::pin(async move {sink.send(value).await}));
                                },
                                protocol::Message::Result{data} if opened && sink.is_none() && output.is_none()=>{
                                    output=Some(protocol::value(data.get().as_bytes(),operation.result.as_ref().expect("result"),&token)?);
                                    finite_confirmed=true;
                                },
                                protocol::Message::Control{id,outcome} if opened=>{
                                    let (expected,reply,_)=pending.take().ok_or_else(||error("Unsolicited session control receipt"))?;
                                    if id!=expected {pending=Some((expected,reply,until)); return Err(error("Session control receipt belongs to another action"));}
                                    let outcome=match outcome {
                                        protocol::Receipt::Confirmed{data}=>{
                                            let control=operation.control.as_ref().expect("declared control");
                                            match protocol::value(data.get().as_bytes(),&control.output,&token) {
                                                Ok(v)=>{
                                                    let provenance=v.provenance().clone().with_policy(&control.policy.policy());
                                                    ControlOutcome::Confirmed(v.with_provenance(provenance))
                                                },
                                                Err(_)=>{ control_unconfirmed=true; let _=reply.send(ControlOutcome::Unknown); return Err(error("Session control receipt violates its captured contract; outcome is unknown"));}
                                            }
                                        },
                                        protocol::Receipt::Refused=>ControlOutcome::Refused,
                                        protocol::Receipt::Unknown=>ControlOutcome::Unknown,
                                    };
                                    let _=reply.send(outcome);
                                },
                                protocol::Message::End if opened && pending.is_none() && (sink.is_some() || output.is_some())=>{
                                    ended=true;
                                    if delivery.is_none() {return Ok(());}
                                },
                                protocol::Message::Failed=>{
                                    finite_confirmed=true;
                                    return Err(error("External provider reported a session failure; no retry was performed"));
                                },
                                _=>return Err(error("Session frame violates the captured operation state")),
                            }
                        }
                    }
                }
            };
            tokio::select! {biased;
                ()=availability.cancelled()=>Err(InvocationError::Cancelled),
                work=work=>work,
            }
        };
        // No control receipt releases physical ownership until the exact subprocess,
        // accepted writes and both bounded readers have ended. Failure never restarts.
        drop(stdin);
        drop(delivery);
        let cleanup = child.cancel().await;
        io_stop.cancel();
        let reader_joined = reader.await.is_ok();
        if !diagnostics_done {
            diagnostics_failed = !matches!(diagnostics.await, Ok(Ok(())));
        }
        control_unconfirmed |= pending.is_some();
        if let Some((_, reply, _)) = pending {
            let _ = reply.send(ControlOutcome::Unknown);
        }
        while let Ok(request) = requests.try_recv() {
            let _ = request.reply.send(ControlOutcome::Refused);
        }
        cleanup.map_err(|_| unknown())?;
        let work = if !reader_joined || diagnostics_failed {
            Err(error("Session reader cleanup or diagnostic budget failed"))
        } else {
            work
        };
        work.map_err(|reason| {
            if (sink.is_none() && start_handed_off && !finite_confirmed) || control_unconfirmed {
                unknown()
            } else {
                reason
            }
        })?;
        Ok(output)
    }
}
impl Invoker for Session {
    fn invoke(&self, call: Call, token: CancellationToken) -> InvocationFuture {
        let this = self.clone();
        Box::pin(async move {
            this.execute(call, None, token)
                .await?
                .ok_or_else(|| error("Session omitted its finite result"))
        })
    }
}
impl StreamingInvoker for Session {
    fn subscribe(&self, call: Call, sink: StreamSink, token: CancellationToken) -> StreamFuture {
        let this = self.clone();
        Box::pin(async move { this.execute(call, Some(sink), token).await.map(|_| ()) })
    }
}
struct Request {
    action: Value,
    reply: oneshot::Sender<ControlOutcome>,
    cancellation: CancellationToken,
}
struct Controller {
    session: Session,
    send: mpsc::Sender<Request>,
    credentials: CredentialSnapshot,
}
impl LiveController for Controller {
    fn authorize(&self, caller: &InvocationAuthority, action: &Value) -> bool {
        self.session.permits(caller, action) && self.session.credentials_current(&self.credentials)
    }
    fn invoke(&self, action: Value, cancellation: CancellationToken) -> ControlFuture {
        let send = self.send.clone();
        Box::pin(async move {
            let (reply, receive) = oneshot::channel();
            if send
                .try_send(Request {
                    action,
                    reply,
                    cancellation,
                })
                .is_err()
            {
                return ControlOutcome::Refused;
            }
            receive.await.unwrap_or(ControlOutcome::Unknown)
        })
    }
}
