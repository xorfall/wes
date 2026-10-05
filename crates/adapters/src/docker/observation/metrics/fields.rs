use super::*;
type Fields = indexmap::IndexMap<String, Data>;
fn common_shape() -> Vec<(&'static str, Shape)> {
    vec![
        ("container", primitive(Primitive::Text)),
        ("sequence", primitive(Primitive::Int)),
        ("received_at_ns", primitive(Primitive::Int)),
        ("timestamp_ns", option(primitive(Primitive::Int))),
    ]
}
pub(super) fn stats_shape() -> Shape {
    let mut fields = common_shape();
    for name in [
        "previous_timestamp_ns",
        "cpu_total_ns",
        "previous_cpu_total_ns",
        "system_cpu_ns",
        "previous_system_cpu_ns",
        "online_cpus",
        "memory_usage_bytes",
        "memory_limit_bytes",
        "memory_cache_bytes",
        "memory_working_set_bytes",
    ] {
        fields.push((name, option(primitive(Primitive::Int))));
    }
    fields.extend([
        ("cpu_percent", option(primitive(Primitive::Decimal))),
        ("cpu_unavailable", option(primitive(Primitive::Text))),
        ("memory_percent", option(primitive(Primitive::Decimal))),
        ("memory_unavailable", option(primitive(Primitive::Text))),
        ("memory_cache_source", option(primitive(Primitive::Text))),
    ]);
    record("DockerStatsSample", fields)
}
pub(super) fn event_shape() -> Shape {
    let mut fields = common_shape();
    fields.extend([
        ("type", primitive(Primitive::Text)),
        ("action", primitive(Primitive::Text)),
    ]);
    record("DockerContainerEvent", fields)
}
fn opt(value: Option<Data>) -> Data {
    Data::Option(value.map(Box::new))
}
fn put_int(fields: &mut Fields, name: &str, value: Option<i64>) {
    fields.insert(name.into(), opt(value.map(Data::Int)));
}
fn put_text(fields: &mut Fields, name: &str, value: Option<&str>) {
    fields.insert(name.into(), opt(value.map(|s| Data::Text(s.into()))));
}
fn common(id: &str, sequence: i64, received: i64, timestamp: Option<i64>) -> Fields {
    [
        ("container".into(), Data::Text(id.into())),
        ("sequence".into(), Data::Int(sequence)),
        ("received_at_ns".into(), Data::Int(received)),
        ("timestamp_ns".into(), opt(timestamp.map(Data::Int))),
    ]
    .into()
}
fn number(json: &Json, path: &str) -> Result<Option<i64>, InvocationError> {
    match json.pointer(path).filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(value) => value
            .as_i64()
            .filter(|n| *n >= 0)
            .map(Some)
            .ok_or_else(malformed),
    }
}
fn percent(numerator: i64, denominator: i64, multiplier: i64) -> Data {
    // Explicit six-decimal rounding of a bounded integer ratio, no floating-point NaN/Inf.
    let scaled = i128::from(numerator) * i128::from(multiplier) * 100_000_000;
    let rounded = (scaled + i128::from(denominator) / 2) / i128::from(denominator);
    Data::Decimal(
        format!("{}.{:06}", rounded / 1_000_000, rounded % 1_000_000)
            .parse()
            .expect("bounded decimal ratio"),
    )
}
pub(super) fn stats(
    json: &Json,
    id: &str,
    sequence: i64,
    received: i64,
) -> Result<Fields, InvocationError> {
    if json.get("id").and_then(Json::as_str) != Some(id) {
        return Err(failure(
            "DOCKER_IDENTITY",
            "Docker stats returned a different or missing container identity",
        ));
    }
    let time = timestamp(&json["read"]);
    let previous_time = timestamp(&json["preread"]);
    let mut fields = common(id, sequence, received, time);
    let total = number(json, "/cpu_stats/cpu_usage/total_usage")?;
    let previous = number(json, "/precpu_stats/cpu_usage/total_usage")?;
    let system = number(json, "/cpu_stats/system_cpu_usage")?;
    let previous_system = number(json, "/precpu_stats/system_cpu_usage")?;
    let cpus = match number(json, "/cpu_stats/online_cpus")? {
        Some(n) => Some(n),
        None => match json
            .pointer("/cpu_stats/cpu_usage/percpu_usage")
            .filter(|v| !v.is_null())
        {
            None => None,
            Some(v) => Some(v.as_array().ok_or_else(malformed)?.len() as i64),
        },
    };
    let (cpu, reason) = match (
        time,
        previous_time,
        total,
        previous,
        system,
        previous_system,
        cpus,
    ) {
        (Some(t), Some(p), Some(c), Some(pc), Some(s), Some(ps), Some(n))
            if p > 0 && t > p && c >= pc && s > ps && ps > 0 && n > 0 && n <= 1_000_000 =>
        {
            (Some(percent(c - pc, s - ps, n)), None)
        }
        (_, _, None, _, _, _, _)
        | (_, _, _, None, _, _, _)
        | (_, _, _, _, None, _, _)
        | (_, _, _, _, _, None, _) => (None, Some("CPU counters unavailable")),
        (_, _, _, _, _, _, None | Some(0)) => (None, Some("CPU count unavailable")),
        _ => (
            None,
            Some(
                "CPU requires increasing sample time and system counter, nondecreasing usage, and a valid prior sample",
            ),
        ),
    };
    for (name, value) in [
        ("previous_timestamp_ns", previous_time),
        ("cpu_total_ns", total),
        ("previous_cpu_total_ns", previous),
        ("system_cpu_ns", system),
        ("previous_system_cpu_ns", previous_system),
        ("online_cpus", cpus),
    ] {
        put_int(&mut fields, name, value);
    }
    fields.insert("cpu_percent".into(), opt(cpu));
    put_text(&mut fields, "cpu_unavailable", reason);
    let usage = number(json, "/memory_stats/usage")?;
    let limit = number(json, "/memory_stats/limit")?;
    let mut cache = None;
    let mut cache_source = None;
    for name in ["total_inactive_file", "inactive_file", "cache"] {
        if let Some(value) = number(json, &format!("/memory_stats/stats/{name}"))? {
            cache = Some(value);
            cache_source = Some(name);
            break;
        }
    }
    let working = usage
        .zip(cache)
        .and_then(|(u, c)| u.checked_sub(c).filter(|v| *v >= 0));
    let memory_percent = working
        .zip(limit)
        .filter(|(_, l)| *l > 0)
        .map(|(u, l)| percent(u, l, 1));
    let reason = if usage.is_none() {
        Some("Memory usage unavailable")
    } else if cache.is_none() {
        Some("Memory cache counter unavailable; working set cannot be derived")
    } else if working.is_none() {
        Some("Memory cache exceeds usage; inconsistent sample")
    } else if limit.is_none_or(|n| n == 0) {
        Some("Memory limit unavailable or zero")
    } else {
        None
    };
    for (name, value) in [
        ("memory_usage_bytes", usage),
        ("memory_limit_bytes", limit),
        ("memory_cache_bytes", cache),
        ("memory_working_set_bytes", working),
    ] {
        put_int(&mut fields, name, value);
    }
    fields.insert("memory_percent".into(), opt(memory_percent));
    put_text(&mut fields, "memory_unavailable", reason);
    put_text(&mut fields, "memory_cache_source", cache_source);
    Ok(fields)
}
pub(super) fn event(
    json: &Json,
    id: &str,
    sequence: i64,
    received: i64,
) -> Result<Fields, InvocationError> {
    if json.pointer("/Actor/ID").and_then(Json::as_str) != Some(id)
        || json.get("Type").and_then(Json::as_str) != Some("container")
    {
        return Err(failure(
            "DOCKER_IDENTITY",
            "Docker event did not match the requested container/type filter",
        ));
    }
    let action = text(&json["Action"])?;
    if matches!(&action, Data::Text(s) if s.is_empty()) {
        return Err(malformed());
    }
    let time = match number(json, "/timeNano")? {
        Some(t) => Some(t),
        None => number(json, "/time")?
            .map(|s| s.checked_mul(1_000_000_000).ok_or_else(malformed))
            .transpose()?,
    };
    let mut fields = common(id, sequence, received, time);
    fields.insert("type".into(), Data::Text("container".into()));
    fields.insert("action".into(), action);
    Ok(fields)
}
