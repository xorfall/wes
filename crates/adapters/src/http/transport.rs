//! One HTTP contract; target execution and protocol interpretation remain separate.
use super::{Failure, HttpConfig, auth::Redactor, inspection};
use reqwest::{Request, header::HeaderMap};
use wes_core::environments::{Binding, TargetKind};
use wes_engine::{
    driver::CancellationToken, environments::Authority, providers::InvocationError,
    trace::TraceSink,
};

#[derive(Clone)]
pub(crate) enum Transport {
    Internal,
    Curl {
        binding: Binding,
        authority: Authority,
    },
}
#[derive(Debug)]
pub(super) struct Response {
    pub status: u16,
    pub version: String,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}
impl Transport {
    pub(crate) fn bound(binding: &Binding, authority: Authority) -> Result<Self, &'static str> {
        match (
            binding.import().declaration().transport.as_deref(),
            binding.import().target().kind(),
        ) {
            (None | Some("internal"), TargetKind::Local) => Ok(Self::Internal),
            (Some("curl"), _) if cfg!(unix) => Ok(Self::Curl {
                binding: binding.clone(),
                authority,
            }),
            (Some("curl"), _) => {
                Err("curl transport requires a POSIX target and host in this version")
            }
            (None, _) => Err(
                "Remote HTTP requires bind.transport: curl; install curl on the selected target",
            ),
            (Some("internal"), _) => Err(
                "internal HTTP runs only on a local target; select transport: curl for remote HTTP",
            ),
            _ => Err("Unsupported HTTP transport; use internal or curl"),
        }
    }
    pub(super) async fn send(
        &self,
        client: &reqwest::Client,
        request: Request,
        config: HttpConfig,
        token: CancellationToken,
        trace: Option<&TraceSink>,
        redactor: &Redactor,
    ) -> Result<Response, InvocationError> {
        match self {
            Self::Internal => {
                let work = async {
                    let response = client.execute(request).await.map_err(Failure::transport)?;
                    if let Some(trace) = trace {
                        inspection::response(trace, &response, redactor);
                    }
                    let headers = response.headers().clone();
                    let version = format!("{:?}", response.version());
                    let (status, body) =
                        super::read_body(response, config.response.bytes, trace).await?;
                    Ok(Response {
                        status,
                        version,
                        headers,
                        body,
                    })
                };
                tokio::select! { biased;
                    () = token.cancelled() => Err(InvocationError::Cancelled),
                    result = tokio::time::timeout(config.request_timeout, work) => result.map_err(|_| Failure::Timeout.error())?.map_err(Failure::error),
                }
            }
            Self::Curl { binding, authority } => {
                let result = super::curl::send(binding, authority, request, config, token).await?;
                if let Some(trace) = trace {
                    use wes_engine::trace::{integer, record, text};
                    trace.emit(
                        "http.response",
                        record(
                            "HttpResponseHead",
                            [
                                ("status".into(), integer(result.status.into())),
                                ("version".into(), text(&result.version)),
                                ("headerCount".into(), integer(result.headers.len() as i64)),
                                (
                                    "headers".into(),
                                    inspection::headers(&result.headers, Some(redactor)),
                                ),
                            ],
                            Default::default(),
                        ),
                    );
                }
                Ok(result)
            }
        }
    }
}
