//! Finite process acquisition; pipe buffers never grow with total output.
use super::*;
use wes_engine::storage::{
    AcquiredFuture, Acquisition, AcquisitionContext, CHUNK_BYTES, LeafSelection, acquisition_error,
};
type PendingAppend<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<(), wes_engine::storage::StoreError>> + Send + 'a>,
>;
pub(super) fn invoke(
    this: ProcessInvoker,
    call: Call,
    cancellation: CancellationToken,
    context: AcquisitionContext,
) -> AcquiredFuture {
    Box::pin(async move {
        if cancellation.is_cancelled() {
            return Err(InvocationError::Cancelled);
        }
        let acquisition = context
            .begin(wes_budgets::get("storage.acquisition.bytes"))
            .await
            .map_err(acquisition_error)?;
        let launched = this.launch_call(&call, &cancellation).await;
        let (mut child, deadline, line) = match launched {
            Ok(v) => v,
            Err(e) => {
                let _ = acquisition.abort().await;
                return Err(e);
            }
        };
        let mut pending = None;
        match capture_owned(
            child.inner(),
            deadline,
            &cancellation,
            &acquisition,
            &mut pending,
        )
        .await
        {
            Ok((status, stderr)) => {
                drop(pending);
                if cancellation.is_cancelled() {
                    let reaped = child.cancel().await;
                    let _ = acquisition.abort().await;
                    reaped.map_err(|e| Failure::Cleanup(e).error())?;
                    return Err(InvocationError::Cancelled);
                }
                child.complete();
                let shell = Value::new(
                    output_shape(),
                    Data::Record(IndexMap::from([
                        ("exitCode".into(), Data::Int(exit_code(status))),
                        ("stdout".into(), Data::Bytes(vec![].into())),
                        ("stderr".into(), Data::Bytes(stderr.into())),
                    ])),
                    Provenance::default().with_fact(
                        "ranLocally",
                        if this.launch.is_some() {
                            "managed local process".into()
                        } else {
                            line.join(" ")
                        },
                    ),
                )
                .map_err(|_| Failure::Internal.error())?;
                acquisition
                    .finish(
                        shell,
                        vec![
                            LeafSelection {
                                port: "stdout".into(),
                                fields: vec!["stdout".into()],
                                text: false,
                            },
                            LeafSelection {
                                port: "stderr".into(),
                                fields: vec!["stderr".into()],
                                text: false,
                            },
                        ],
                    )
                    .await
            }
            Err(reason) => {
                let reaped = child.cancel().await;
                // Cancellation stops the process independently of storage progress. An accepted
                // append remains owned until its physical acknowledgement, before abort/exit.
                if let Some(append) = pending.take() {
                    let _ = append.await;
                }
                drop(pending);
                let aborted = acquisition.abort().await;
                reaped.map_err(|e| Failure::Cleanup(e).error())?;
                if let Err(e) = aborted {
                    return Err(acquisition_error(e));
                }
                if cancellation.is_cancelled() {
                    Err(InvocationError::Cancelled)
                } else {
                    Err(reason)
                }
            }
        }
    })
}
async fn capture_owned<'a>(
    child: &mut Child,
    deadline: Instant,
    cancellation: &CancellationToken,
    acquisition: &'a Acquisition,
    pending: &mut Option<PendingAppend<'a>>,
) -> Result<(ExitStatus, Vec<u8>), InvocationError> {
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let mut status = None;
    let mut out_buf = [0u8; CHUNK_BYTES];
    let mut err_buf = [0u8; CHUNK_BYTES];
    let mut excerpt = Vec::with_capacity(2049);
    loop {
        if let Some(status) = status
            && stdout.is_none()
            && stderr.is_none()
            && pending.is_none()
        {
            return Ok((status, excerpt));
        }
        let wake = deadline.min(Instant::now() + Duration::from_secs(86_400));
        tokio::select! {
            biased;
            _=cancellation.cancelled()=>return Err(InvocationError::Cancelled),
            _=tokio::time::sleep_until(wake)=>if Instant::now()>=deadline{return Err(Failure::Timeout.error());},
            waited=child.wait(),if status.is_none()=>status=Some(waited.map_err(|_|Failure::Wait.error())?),
            appended=async {pending.as_mut().expect("pending append").await},if pending.is_some()=>{pending.take();appended.map_err(acquisition_error)?;},
            read=read_pipe(&mut stdout,&mut out_buf),if pending.is_none()=>{let n=read.map_err(|_|Failure::Read.error())?;if n==0{stdout=None;}else{*pending=Some(Box::pin(acquisition.append("stdout",out_buf[..n].to_vec())));}},
            read=read_pipe(&mut stderr,&mut err_buf),if pending.is_none()=>{let n=read.map_err(|_|Failure::Read.error())?;if n==0{stderr=None;}else{let take=n.min(2049-excerpt.len());excerpt.extend_from_slice(&err_buf[..take]);*pending=Some(Box::pin(acquisition.append("stderr",err_buf[..n].to_vec())));}},
        }
    }
}
