//! Finite external effects. One dispatch, bounded receipts, no remote rollback or hidden retries.
use super::*;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) fn capabilities() -> Vec<Capability> {
    let text = || primitive(Primitive::Text);
    let integer = || primitive(Primitive::Int);
    let mut pull = Capability::new(["image", "pull"], image_shape(), Safety::Unsafe);
    pull.summary = "Pull one public image reference; consume bounded progress locally and return its resolved sha256 ID. No credentials or automatic retry".into();
    pull.parameters = vec![Parameter::new("reference", text(), true)];
    let mut create = Capability::new(["container", "create"], created_shape(), Safety::Unsafe);
    create.summary = "Create a stopped container from an exact sha256 image ID and explicit argv. Network none; no mounts, image volumes, devices or privileges. Bounded memory/CPU/PIDs; generated name; no automatic cleanup".into();
    create.parameters = vec![
        Parameter::new("image", text(), true),
        Parameter::new("argv", Shape::List(Box::new(text())), true),
        Parameter::new("memory_mb", integer(), false),
        Parameter::new("cpu_millis", integer(), false),
        Parameter::new("pids", integer(), false),
    ];
    let mut caps = vec![pull, create];
    for (operation, summary) in [
        (
            "start",
            "Start an exact container ID in your live creation scope (local user may select an existing ID)",
        ),
        (
            "stop",
            "Stop an exact container ID in your live creation scope, with a 10 second grace period",
        ),
        (
            "remove",
            "Remove a stopped exact container ID in your live creation scope; never force or delete volumes",
        ),
    ] {
        let mut cap = Capability::new(["container", operation], receipt_shape(), Safety::Unsafe);
        cap.summary = summary.into();
        cap.parameters = vec![Parameter::new("container", text(), true).suggesting("container")];
        caps.push(cap);
    }
    caps
}
fn image_shape() -> Shape {
    record(
        "DockerImageReceipt",
        vec![
            ("image", primitive(Primitive::Text)),
            ("reference", primitive(Primitive::Text)),
            ("run", primitive(Primitive::Text)),
        ],
    )
}
fn receipt_shape() -> Shape {
    record(
        "DockerContainerReceipt",
        vec![
            ("container", primitive(Primitive::Text)),
            ("operation", primitive(Primitive::Text)),
            ("run", primitive(Primitive::Text)),
        ],
    )
}
fn created_shape() -> Shape {
    record(
        "DockerContainerCreated",
        vec![
            ("container", primitive(Primitive::Text)),
            ("name", primitive(Primitive::Text)),
            ("image", primitive(Primitive::Text)),
            ("run", primitive(Primitive::Text)),
            (
                "warnings",
                Shape::List(Box::new(primitive(Primitive::Text))),
            ),
            ("warnings_truncated", primitive(Primitive::Bool)),
        ],
    )
}
fn result(
    shape: Shape,
    pairs: impl IntoIterator<Item = (&'static str, String)>,
) -> Result<Value, InvocationError> {
    Value::new(
        shape,
        Data::Record(
            pairs
                .into_iter()
                .map(|(k, v)| (k.into(), Data::Text(v.into())))
                .collect(),
        ),
        Provenance::default().with_fact("docker.api", "1.45"),
    )
    .map_err(|_| malformed())
}
fn text<'a>(call: &'a Call, key: &str) -> Result<&'a str, InvocationError> {
    match call.arguments.get(key).map(Value::data) {
        Some(Data::Text(s)) => Ok(s),
        _ => Err(failure("DOCKER_ARGUMENT", format!("{key} requires Text"))),
    }
}
fn image_id(id: &str) -> bool {
    id.strip_prefix("sha256:").is_some_and(digest)
}
fn argument(message: &str) -> InvocationError {
    failure("DOCKER_ARGUMENT", message)
}
fn unknown(call: &Call) -> InvocationError {
    let locator = if call.capability.path == ["container", "create"] {
        format!(" Generated name: {}.", creation_name(call))
    } else {
        String::new()
    };
    failure(
        wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN,
        format!(
            "Docker {} outcome is unknown after dispatch (run {}).{} Remote changes may have occurred. Inspect the selected daemon before explicitly retrying; no automatic retry or cleanup was performed.",
            call.capability.path.join(" "),
            call.run.id(),
            locator
        ),
    )
}
fn creation_name(call: &Call) -> String {
    format!(
        "wes-{:x}",
        Sha256::digest(call.run.id().as_str().as_bytes())
    )
}
fn denied(message: &'static str) -> InvocationError {
    failure("AUT001", message)
}
impl Observation {
    pub(super) async fn effect(
        &self,
        call: Call,
        token: CancellationToken,
    ) -> Result<Value, InvocationError> {
        // Confidential data must not be copied into daemon metadata, image references or argv.
        if call
            .arguments
            .values()
            .any(|v| v.provenance().policy().is_confidential())
        {
            return Err(failure(
                "ENV030",
                "Docker lifecycle does not support confidential arguments",
            ));
        }
        self.permitted()?;
        let lease = self
            .authority
            .availability_lease(self.binding.environment().identity())
            .map_err(|_| denied("Docker environment authority is disabled"))?;
        let dispatched = AtomicBool::new(false);
        let timeout =
            Duration::from_millis(self.binding.import().timeout_ms().unwrap_or(300_000) as u64);
        tokio::select! { biased;
            () = token.cancelled() => Err(if dispatched.load(Ordering::Acquire) { unknown(&call) } else { InvocationError::Cancelled }),
            () = lease.cancelled() => Err(if dispatched.load(Ordering::Acquire) { unknown(&call) } else { denied("Docker environment authority ended before dispatch") }),
            value = tokio::time::timeout(timeout, self.mutate(&call, &dispatched, &lease)) => value.unwrap_or_else(|_| Err(if dispatched.load(Ordering::Acquire) { unknown(&call) } else { failure("DOCKER_TIMEOUT", "Docker timed out before dispatch; no mutation was requested") })),
        }
    }
    // Every effect funnels through one boundary. No mutation uses the read-only transport helper.
    async fn dispatch(
        &self,
        request: reqwest::RequestBuilder,
        call: &Call,
        dispatched: &AtomicBool,
        lease: &CancellationToken,
    ) -> Result<reqwest::Response, InvocationError> {
        if lease.is_cancelled() {
            return Err(denied("Docker environment authority ended before dispatch"));
        }
        self.permitted()?;
        dispatched.store(true, Ordering::Release);
        let response = request.send().await.map_err(|_| {
            self.client.invalidate();
            unknown(call)
        })?;
        let status = response.status().as_u16();
        let expected = match call.capability.path.last().map(String::as_str) {
            Some("pull") => status == 200,
            Some("create") => status == 201,
            Some("start" | "stop") => status == 204 || status == 304,
            Some("remove") => status == 204,
            _ => false,
        };
        if expected {
            return Ok(response);
        }
        if response.status().is_success() || status == 304 || status == 408 || status >= 500 {
            return Err(unknown(call));
        }
        Err(failure(
            "DOCKER_HTTP",
            match status {
                404 => {
                    "Docker rejected the request: exact image/container is missing. Refresh inventory and select explicitly; no replacement was followed."
                }
                409 => {
                    "Docker rejected the request due to a conflict. Container removal requires it to be stopped; inspect its state before another explicit action."
                }
                401 | 403 => {
                    "Docker denied this operation. Check access to the selected daemon; authenticated registry pulls are not supported."
                }
                _ => {
                    "Docker rejected the operation. Check its help and the selected daemon; no automatic retry was performed."
                }
            },
        ))
    }
    async fn mutate(
        &self,
        call: &Call,
        dispatched: &AtomicBool,
        lease: &CancellationToken,
    ) -> Result<Value, InvocationError> {
        let env = self.binding.environment().identity();
        match call
            .capability
            .path
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice()
        {
            ["image", "pull"] => {
                self.authority
                    .check_external_creation(&call.authority, env)
                    .map_err(denied)?;
                let reference = text(call, "reference")?;
                if reference.is_empty()
                    || reference.len() > 512
                    || reference.starts_with('-')
                    || !reference
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"/._-:@".contains(&c))
                {
                    return Err(argument(
                        "reference must be one public registry image name with an explicit tag or digest",
                    ));
                }
                let tail = reference.rsplit('/').next().unwrap_or("");
                let explicit = if let Some((repository, hash)) = reference.split_once('@') {
                    !repository.is_empty() && image_id(hash)
                } else {
                    tail.rsplit_once(':')
                        .is_some_and(|(repository, tag)| !repository.is_empty() && !tag.is_empty())
                };
                if !explicit {
                    return Err(argument(
                        "reference requires an explicit tag or sha256 digest; implicit latest and all-tags pulls are unsupported",
                    ));
                }
                let api = self.client.endpoint().await.map_err(transport)?;
                let mut response = self
                    .dispatch(
                        self.client.http.post(query(
                            &format!("{api}/images/create"),
                            &[("fromImage", reference)],
                        )),
                        call,
                        dispatched,
                        lease,
                    )
                    .await?;
                let mut pending = Vec::new();
                let mut total = 0usize;
                let mut records = 0usize;
                while let Some(chunk) = response.chunk().await.map_err(|_| unknown(call))? {
                    total += chunk.len();
                    if total > 8 * 1024 * 1024 {
                        return Err(unknown(call));
                    }
                    for byte in chunk {
                        if byte == b'\n' {
                            if !pending.is_empty() {
                                progress(&pending, call)?;
                                records += 1;
                                pending.clear();
                            }
                        } else {
                            if pending.len() >= 64 * 1024 {
                                return Err(unknown(call));
                            }
                            pending.push(byte);
                        }
                    }
                }
                if !pending.is_empty() {
                    progress(&pending, call)?;
                    records += 1;
                }
                if records == 0 {
                    return Err(unknown(call));
                }
                // Resolve once after the pull. Subsequent creation uses only this immutable ID.
                let mut url = reqwest::Url::parse(&format!("{api}/images/")).expect("static URL");
                url.path_segments_mut()
                    .expect("base URL")
                    .pop_if_empty()
                    .push(reference)
                    .push("json");
                let image = self
                    .client
                    .json(self.client.http.get(url))
                    .await
                    .map_err(|_| unknown(call))?;
                let id = image
                    .get("Id")
                    .and_then(Json::as_str)
                    .filter(|id| image_id(id))
                    .ok_or_else(|| unknown(call))?;
                result(
                    image_shape(),
                    [
                        ("image", id.into()),
                        ("reference", reference.into()),
                        ("run", call.run.id().to_string()),
                    ],
                )
            }
            ["container", "create"] => {
                let image = text(call, "image")?;
                if !image_id(image) {
                    return Err(argument(
                        "image requires an exact sha256: followed by 64 hexadecimal characters; use image pull's image result",
                    ));
                }
                let argv = match call.arguments.get("argv").map(Value::data) {
                    Some(Data::List(args)) if !args.is_empty() && args.len() <= 128 => args.iter().map(|a| match a { Data::Text(t) if t.len() <= 8192 && !t.contains('\0') => Ok(t.to_string()), _ => Err(argument("argv must contain 1..128 text arguments, each at most 8192 bytes without NUL")) }).collect::<Result<Vec<_>,_>>()?,
                    _ => return Err(argument("argv requires a nonempty List<Text>; no shell is inserted")),
                };
                if argv[0].is_empty() || argv.iter().map(String::len).sum::<usize>() > 64 * 1024 {
                    return Err(argument("argv exceeds 64 KiB or has an empty executable"));
                }
                let number = |key, default, min, max| match call.arguments.get(key).map(Value::data)
                {
                    None => Ok(default),
                    Some(Data::Int(n)) if (min..=max).contains(n) => Ok(*n),
                    _ => Err(failure(
                        "DOCKER_ARGUMENT",
                        format!("{key} must be between {min} and {max}"),
                    )),
                };
                let memory = number("memory_mb", 256, 16, 4096)?;
                let cpu = number("cpu_millis", 500, 100, 4000)?;
                let pids = number("pids", 128, 16, 1024)?;
                let reservation = self
                    .authority
                    .reserve_resource(&call.authority, env, &self.connection)
                    .map_err(denied)?;
                let api = self.client.endpoint().await.map_err(transport)?;
                let metadata = self
                    .client
                    .json(self.client.http.get(format!("{api}/images/{image}/json")))
                    .await
                    .map_err(transport)?;
                if metadata.get("Id").and_then(Json::as_str) != Some(image) {
                    return Err(failure(
                        "DOCKER_IDENTITY",
                        "Docker returned a different image identity; no container was created",
                    ));
                }
                // Image-declared volumes would create anonymous mounts even without HostConfig binds.
                match metadata.pointer("/Config/Volumes") {
                    None | Some(Json::Null) => (),
                    Some(Json::Object(v)) if v.is_empty() => (),
                    _ => {
                        return Err(argument(
                            "Image declares volumes; this lifecycle slice does not create mounts or anonymous volumes. Select an image without VOLUME declarations",
                        ));
                    }
                }
                let name = creation_name(call);
                let payload = json!({"Image":image,"Entrypoint":argv,"Cmd":[],"Tty":false,"OpenStdin":false,"Healthcheck":{"Test":["NONE"]},"Labels":{"wes.run":call.run.id().as_str()},"HostConfig":{"NetworkMode":"none","Memory":memory*1024*1024,"MemorySwap":memory*1024*1024,"NanoCpus":cpu*1_000_000,"PidsLimit":pids,"Privileged":false,"CapDrop":["ALL"],"SecurityOpt":["no-new-privileges:true"],"AutoRemove":false,"RestartPolicy":{"Name":"no"}}});
                let response = self
                    .dispatch(
                        self.client
                            .http
                            .post(query(
                                &format!("{api}/containers/create"),
                                &[("name", &name)],
                            ))
                            .header("content-type", "application/json")
                            .body(payload.to_string()),
                        call,
                        dispatched,
                        lease,
                    )
                    .await?;
                let bytes = super::super::bounded(response, super::super::client::METADATA)
                    .await
                    .map_err(|_| unknown(call))?;
                let response: Json = serde_json::from_slice(&bytes).map_err(|_| unknown(call))?;
                let id = response
                    .get("Id")
                    .and_then(Json::as_str)
                    .filter(|s| digest(s))
                    .ok_or_else(|| unknown(call))?;
                reservation.record(id).map_err(|_| unknown(call))?;
                let raw = response
                    .get("Warnings")
                    .and_then(Json::as_array)
                    .ok_or_else(|| unknown(call))?;
                if raw.iter().any(|w| !w.is_string()) {
                    return Err(unknown(call));
                }
                let mut truncated = raw.len() > 16;
                let warnings = raw
                    .iter()
                    .take(16)
                    .map(|w| {
                        let text = w.as_str().expect("validated string");
                        let mut end = text.len().min(1024);
                        while !text.is_char_boundary(end) {
                            end -= 1;
                        }
                        truncated |= end < text.len();
                        Data::Text(text[..end].into())
                    })
                    .collect();
                let mut data: indexmap::IndexMap<String, Data> = [
                    ("container", id.into()),
                    ("name", name),
                    ("image", image.into()),
                    ("run", call.run.id().to_string()),
                ]
                .into_iter()
                .map(|(k, v)| (k.into(), Data::Text(v.into())))
                .collect();
                data.insert("warnings".into(), Data::List(warnings));
                data.insert("warnings_truncated".into(), Data::Bool(truncated));
                Value::new(
                    created_shape(),
                    Data::Record(data),
                    Provenance::default().with_fact("docker.api", "1.45"),
                )
                .map_err(|_| unknown(call))
            }
            ["container", operation @ ("start" | "stop" | "remove")] => {
                let id = text(call, "container")?;
                if !digest(id) {
                    return Err(argument(
                        "container requires a full 64-character Docker ID; names and prefixes cannot select mutation targets",
                    ));
                }
                self.authority
                    .check_resource(&call.authority, env, &self.connection, id)
                    .map_err(denied)?;
                let api = self.client.endpoint().await.map_err(transport)?;
                let request = match *operation {
                    "start" => self
                        .client
                        .http
                        .post(format!("{api}/containers/{id}/start")),
                    "stop" => self.client.http.post(query(
                        &format!("{api}/containers/{id}/stop"),
                        &[("t", "10")],
                    )),
                    _ => self.client.http.delete(query(
                        &format!("{api}/containers/{id}"),
                        &[("force", "false"), ("v", "false")],
                    )),
                };
                let response = self.dispatch(request, call, dispatched, lease).await?;
                // Consume the finite response before acknowledging; do not keep a hidden connection.
                super::super::bounded(response, 64 * 1024)
                    .await
                    .map_err(|_| unknown(call))?;
                if *operation == "remove" {
                    self.authority.forget_resource(env, &self.connection, id);
                }
                result(
                    receipt_shape(),
                    [
                        ("container", id.into()),
                        ("operation", (*operation).into()),
                        ("run", call.run.id().to_string()),
                    ],
                )
            }
            _ => Err(argument("Unknown Docker lifecycle operation")),
        }
    }
}
fn progress(bytes: &[u8], call: &Call) -> Result<(), InvocationError> {
    let line: Json = serde_json::from_slice(bytes).map_err(|_| unknown(call))?;
    if !line.is_object() {
        return Err(unknown(call));
    }
    if line.get("error").is_some() || line.get("errorDetail").is_some() {
        return Err(failure(
            "DOCKER_PULL_FAILED",
            "Docker reported an image pull error. Check the public reference and registry access. Downloaded layers may remain; no retry or cleanup was performed",
        ));
    }
    Ok(())
}

fn query(base: &str, pairs: &[(&str, &str)]) -> reqwest::Url {
    let mut url = reqwest::Url::parse(base).expect("validated Docker URL");
    url.query_pairs_mut().extend_pairs(pairs.iter().copied());
    url
}
