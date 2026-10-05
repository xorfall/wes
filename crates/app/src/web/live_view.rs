//! Demand-driven presentation samples, separate from stored/current calculation results.
use super::*;

fn problem(status: StatusCode, code: &'static str, message: impl ToString) -> Response {
    (
        status,
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::CONTENT_TYPE, "application/json"),
        ],
        serde_json::json!({"code": code, "message": message.to_string()}).to_string(),
    )
        .into_response()
}
fn metadata(sample: &wes_engine::session::DisplaySample) -> header::HeaderValue {
    let sources: Vec<_> = sample.sources.iter().map(|source| {
        let counts = source.counts.map(|(omitted, rejected, items)| serde_json::json!({
            "omitted": omitted.to_string(), "rejected": rejected.to_string(), "windowItems": items,
            "accepted": (u128::from(omitted) + items as u128).to_string()
        }));
        serde_json::json!({"node":source.node.as_str(),"run":source.run.as_str(),"phase":source.phase,"counts":counts})
    }).collect();
    let epochs: Vec<_> = sample
        .epochs
        .iter()
        .map(|(node, run)| [node.as_str(), run.as_str()])
        .collect();
    header::HeaderValue::from_str(&serde_json::json!({"revision":sample.revision.to_string(),"producerRun":sample.producer_run.as_ref().map(|run|run.as_str()),"epochs":epochs,"sources":sources}).to_string()).expect("display metadata contains only bounded runtime identities")
}
fn with_metadata(mut response: Response, sample: &wes_engine::session::DisplaySample) -> Response {
    response
        .headers_mut()
        .insert("x-wes-display", metadata(sample));
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}
pub(super) async fn read(
    Scoped(shared): Scoped,
    Path(node): Path<String>,
    request: Request,
) -> Response {
    let Ok(current) = shared.application.current() else {
        return problem(StatusCode::GONE, "withdrawn", "Workspace no longer exists.");
    };
    if request
        .headers()
        .get("X-Wes-Session")
        .and_then(|v| v.to_str().ok())
        != Some(current.generation.as_str())
    {
        return problem(
            StatusCode::CONFLICT,
            "withdrawn",
            "Workspace changed; discard its display samples.",
        );
    }
    let Ok(node) = NodeId::new(node) else {
        return problem(StatusCode::BAD_REQUEST, "withdrawn", "Invalid node.");
    };
    let Ok(permit) = shared.reads.clone().try_acquire_owned() else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "busy",
            "Display readers are busy; read again later.",
        );
    };
    let sample = match current.session.display_value(node.clone()).await {
        Ok(sample) => sample,
        Err(SessionError::Capacity) => {
            return problem(
                StatusCode::TOO_MANY_REQUESTS,
                "capacity",
                "Live view limit reached (16). Close another view and read again.",
            );
        }
        Err(error) => return problem(StatusCode::FORBIDDEN, "withdrawn", error),
    };
    if !shared
        .application
        .current()
        .is_ok_and(|now| now.generation == current.generation)
    {
        return problem(
            StatusCode::CONFLICT,
            "withdrawn",
            "Workspace changed; discard its display samples.",
        );
    }
    if sample.over_budget {
        return with_metadata(
            problem(
                StatusCode::PAYLOAD_TOO_LARGE,
                "budget",
                "Live view exceeds its configured retention charge budget. Reduce the value before viewing it.",
            ),
            &sample,
        );
    }
    let Some(value) = sample.value.clone() else {
        return with_metadata(StatusCode::NO_CONTENT.into_response(), &sample);
    };
    let encoded = shared
        .encoders
        .spawn_blocking(move || {
            let _permit = permit;
            encode_display_value(
                &value,
                Limits {
                    bytes: wes_budgets::get("display.wire.bytes") as usize,
                    nodes: wes_budgets::get("display.wire.nodes") as usize,
                },
            )
        })
        .await;
    if !shared
        .application
        .current()
        .is_ok_and(|now| now.generation == current.generation)
    {
        return problem(
            StatusCode::CONFLICT,
            "withdrawn",
            "Workspace changed; discard its display samples.",
        );
    }
    // A private replacement, deletion or retirement while encoding withdraws this sample.
    match current.session.display_value(node).await {
        Ok(current) if current.epochs == sample.epochs => {}
        _ => {
            return problem(
                StatusCode::FORBIDDEN,
                "withdrawn",
                "Source changed or display permission was withdrawn.",
            );
        }
    }
    with_metadata(
        match encoded {
            Ok(Ok(bytes)) => ([(header::CONTENT_TYPE, "application/json")], bytes).into_response(),
            _ => problem(
                StatusCode::PAYLOAD_TOO_LARGE,
                "budget",
                "Live view exceeds its display budget (512 KiB / 50,000 items). Reduce the value before viewing it.",
            ),
        },
        &sample,
    )
}
