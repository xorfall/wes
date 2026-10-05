//! Desktop-wide data-home control; origin middleware and owned host serialize the transition.
use super::*;
use std::sync::atomic::Ordering;

pub(super) async fn read(State(shared): State<Shared>) -> Response {
    let Some(connection) = &shared.services.data_home else {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    };
    services::management_json(
        StatusCode::OK,
        &serde_json::json!({
            "path":connection.location.path,"identity":connection.location.identity,
            "ready":connection.ready.load(Ordering::Acquire),"warning":connection.warning(),
            "url":format!("http://127.0.0.1:{}/",shared.port)
        }),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Open {
    path: String,
}
pub(super) async fn switch(State(shared): State<Shared>, request: Request) -> Response {
    let Some(connection) = &shared.services.data_home else {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    };
    switch_request(connection, request).await
}
async fn switch_request(
    connection: &crate::data_home::host::Connection,
    request: Request,
) -> Response {
    if request
        .headers()
        .get(header::CONTENT_TYPE)
        .is_none_or(|v| v != "application/json")
    {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let body = match tokio::time::timeout(
        Duration::from_secs(5),
        to_bytes(request.into_body(), 16 * 1024),
    )
    .await
    {
        Ok(Ok(body)) => body,
        _ => return StatusCode::BAD_REQUEST.into_response(),
    };
    let Ok(open) = serde_json::from_slice::<Open>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let result = connection.switch(open.path).await;
    match result {
        Ok(location) => services::management_json(StatusCode::OK, &location),
        Err(message) => services::management_json(
            StatusCode::CONFLICT,
            &serde_json::json!({"message":message}),
        ),
    }
}

/// Startup recovery needs no running workspace and never opens the rejected folder's data.
pub(crate) async fn recovery(
    mut connection: crate::data_home::host::Connection,
) -> io::Result<Server> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let address = listener.local_addr()?;
    connection.location.url = format!("http://{address}/");
    let router = Router::new()
        .route("/data-home", get(recovery_status).post(recovery_switch))
        .fallback(|| async {
            (
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                include_str!("data_home_setup.html"),
            )
        })
        .layer(middleware::from_fn(
            move |request: Request, next: Next| async move {
                if !origin_allowed(request.headers(), address.port()) {
                    return StatusCode::FORBIDDEN.into_response();
                }
                let mut response = next.run(request).await;
                response
                    .headers_mut()
                    .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
                response
                    .headers_mut()
                    .insert("x-content-type-options", "nosniff".parse().unwrap());
                response
            },
        ))
        .with_state(connection);
    let stopped = CancellationToken::new();
    let stop = stopped.clone();
    let task = tokio::spawn(async move { connections(listener, router, stop).await });
    Ok(Server {
        address,
        stopped,
        task,
        terminals: crate::terminal::Manager::default(),
        encoders: tokio_util::task::TaskTracker::new(),
    })
}
async fn recovery_status(State(connection): State<crate::data_home::host::Connection>) -> Response {
    services::management_json(
        StatusCode::OK,
        &serde_json::json!({"path":connection.location.path,"warning":connection.warning(),"setup":true}),
    )
}
async fn recovery_switch(
    State(connection): State<crate::data_home::host::Connection>,
    request: Request,
) -> Response {
    switch_request(&connection, request).await
}
