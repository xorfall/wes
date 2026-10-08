use super::*;
use crate::contracts::{PackageNode as Node, PackageScalarKind as Scalar, read_strict_package};
use indexmap::IndexMap;
type Map = IndexMap<String, Node>;

pub(super) fn parse(yaml: &str) -> Result<Package, EnvironmentError> {
    // Never retain the YAML parser's error text: it can contain caller configuration payloads.
    let root = read_strict_package(yaml).map_err(|_| error("ENV001", "invalid or excessive environment YAML; aliases, tags and duplicate keys are not allowed"))?;
    crate::package_schema::declarations()
        .validate("env.package", &root)
        .map_err(|issue| error(issue.code, issue.message))?;
    let root = map(&root)?;
    if int(required(root, "version")?)? != 1 {
        return Err(error("ENV001", "unsupported environment package version"));
    }
    let mut targets = BTreeMap::new();
    if let Some(node) = root.get("targets") {
        for (key, node) in named(node, MAX_TARGETS)? {
            let target = map(node)?;
            let kind = match text(required(target, "kind")?)? {
                "local" => TargetKind::Local,
                "ssh" => {
                    let port = target.get("port").map(int).transpose()?.unwrap_or(22);
                    if !(1..=65535).contains(&port) {
                        return Err(error("ENV001", "SSH port must be between 1 and 65535"));
                    }
                    TargetKind::Ssh(SshTarget {
                        client: text(required(target, "client")?)?.into(),
                        host: text(required(target, "host")?)?.into(),
                        user: text(required(target, "user")?)?.into(),
                        port: port as u16,
                        identity_file: text(required(target, "identity_file")?)?.into(),
                        known_hosts: text(required(target, "known_hosts")?)?.into(),
                    })
                }
                "docker" => {
                    if text(required(target, "inherit")?)? != "container" {
                        return Err(error(
                            "ENV001",
                            "Docker exec requires explicit inherit: container; host environment is never forwarded",
                        ));
                    }
                    let socket = text(required(target, "socket")?)?;
                    let destination = match (target.get("container"), target.get("compose")) {
                        (Some(value), None) => {
                            let container = text(value)?;
                            if container.is_empty()
                                || container.len() > 128
                                || !container.bytes().all(|b| {
                                    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-')
                                })
                            {
                                return Err(error(
                                    "ENV001",
                                    "Docker container requires an ID or literal container name",
                                ));
                            }
                            DockerDestination::Container(container.into())
                        }
                        (None, Some(value)) => {
                            let selector = map(value)?;
                            let project = text(required(selector, "project")?)?;
                            let service = text(required(selector, "service")?)?;
                            let valid = |s: &str| {
                                !s.is_empty()
                                    && s.len() <= 128
                                    && s.bytes().all(|b| {
                                        b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-')
                                    })
                            };
                            if !valid(project) || !valid(service) {
                                return Err(error(
                                    "ENV001",
                                    "Compose project and service require literal names of 1..128 ASCII letters, digits, '.', '_' or '-'",
                                ));
                            }
                            let replica = selector.get("replica").map(int).transpose()?;
                            if replica.is_some_and(|n| n < 1 || n > u32::MAX as i64) {
                                return Err(error(
                                    "ENV001",
                                    "Compose replica must be an integer between 1 and 4294967295",
                                ));
                            }
                            DockerDestination::Compose {
                                project: project.into(),
                                service: service.into(),
                                replica: replica.map(|n| n as u32),
                            }
                        }
                        _ => {
                            return Err(error(
                                "ENV001",
                                "Docker target requires exactly one of container or compose",
                            ));
                        }
                    };
                    let shell = target.get("shell").map(text).transpose()?;
                    if shell.is_some_and(|s| {
                        !s.starts_with('/') || s.len() > 4096 || s.chars().any(char::is_control)
                    }) {
                        return Err(error(
                            "ENV001",
                            "Docker terminal shell must be an absolute container executable path",
                        ));
                    }
                    let image = target.get("image").map(text).transpose()?;
                    if !socket.starts_with('/')
                        || socket.len() > 4096
                        || socket.chars().any(char::is_control)
                        || image.is_some_and(|s| {
                            s.len() != 71
                                || !s.starts_with("sha256:")
                                || !s[7..].bytes().all(|b| b.is_ascii_hexdigit())
                        })
                    {
                        return Err(error(
                            "ENV001",
                            "invalid Docker socket, container or image constraint",
                        ));
                    }
                    TargetKind::Docker {
                        socket: socket.into(),
                        destination,
                        image: image.map(str::to_owned),
                        shell: shell.map(str::to_owned),
                    }
                }
                _ => return Err(error("ENV001", "unsupported execution target kind")),
            };
            let cwd = target.get("cwd").map(text).transpose()?.map(str::to_owned);
            if cwd
                .as_ref()
                .is_some_and(|s| s.is_empty() || s.len() > 4096 || s.chars().any(char::is_control))
            {
                return Err(error("ENV001", "invalid target working directory"));
            }
            let mut variables = BTreeMap::new();
            if let Some(node) = target.get("env") {
                for (name, value) in named(node, 128)? {
                    if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                        return Err(error("ENV001", "invalid process variable name"));
                    }
                    let value = text(value)?;
                    if value.len() > 64 * 1024 || value.contains('\0') {
                        return Err(error("ENV001", "invalid process variable value"));
                    }
                    variables.insert(name.clone(), value.into());
                }
            }
            targets.insert(
                key.clone(),
                Target {
                    kind,
                    name: key.clone(),
                    cwd,
                    variables,
                },
            );
        }
    }
    let mut definitions = BTreeMap::new();
    for (key, node) in named(required(root, "environments")?, MAX_ENVIRONMENTS)? {
        definitions.insert(key.clone(), definition(node)?);
    }
    Ok(Package {
        package: root.get("package").map(identifier).transpose()?,
        definitions,
        targets,
    })
}
fn definition(node: &Node) -> Result<Definition, EnvironmentError> {
    let m = map(node)?;
    let mut d = Definition {
        owner: m.get("owner").map(identifier).transpose()?,
        ..Definition::default()
    };
    if let Some(n) = m.get("drift") {
        d.drift = boolean(n)?;
    }
    d.id = m.get("id").map(identifier).transpose()?;
    if let Some(n) = m.get("retired") {
        d.retired = boolean(n)?;
    }
    if let Some(n) = m.get("protected") {
        d.protected = boolean(n)?;
    }
    if let Some(n) = m.get("abstract") {
        d.abstract_environment = boolean(n)?;
    }
    if let Some(n) = m.get("extends") {
        let p = map(n)?;
        let parent = identifier(required(p, "env")?)?;
        d.parent = Some(match (p.get("track"), p.get("revision")) {
            (Some(track), None) if text(track)? == "latest" => Parent::Latest(parent),
            (None, Some(revision)) => Parent::Pinned {
                name: parent,
                revision: text(revision)?.parse()?,
            },
            _ => {
                return Err(error(
                    "ENV001",
                    "extends requires exactly one of track: latest or revision",
                ));
            }
        });
    }
    if let Some(n) = m.get("parameters") {
        for (key, n) in named(n, MAX_IMPORTS)? {
            let p = map(n)?;
            let kind = match text(required(p, "type")?)? {
                "Text" => ConfigType::Text,
                "Int" => ConfigType::Int,
                "Bool" => ConfigType::Bool,
                _ => {
                    return Err(error(
                        "ENV001",
                        "configuration type must be Text, Int or Bool",
                    ));
                }
            };
            let default = p.get("default").map(value).transpose()?;
            if default.as_ref().is_some_and(|v| !kind.accepts(v)) {
                return Err(error("ENV003", "configuration default has the wrong type"));
            }
            d.parameters
                .insert(key.clone(), Parameter { kind, default });
        }
    }
    if let Some(n) = m.get("config") {
        for (key, n) in named(n, MAX_IMPORTS)? {
            d.config.insert(key.clone(), value(n)?);
        }
    }
    if let Some(n) = m.get("secretSlots") {
        for (key, n) in named(n, MAX_IMPORTS)? {
            let p = map(n)?;
            d.secret_slots
                .insert(key.clone(), boolean(required(p, "required")?)?);
        }
    }
    if let Some(n) = m.get("secretRefs") {
        for (key, n) in named(n, MAX_IMPORTS)? {
            let reference = text(n)?;
            if reference.is_empty()
                || reference.len() > 256
                || reference
                    .chars()
                    .any(|c| c.is_control() || c.is_whitespace())
            {
                return Err(error("ENV001", "invalid secret reference"));
            }
            d.secret_refs.insert(key.clone(), reference.into());
        }
    }
    if let Some(Node::Sequence(items)) = m.get("targets") {
        if items.len() > MAX_TARGETS {
            return Err(error("ENV007", "too many environment targets"));
        }
        for item in items {
            if !d.targets.insert(identifier(item)?) {
                return Err(error("ENV001", "duplicate environment target"));
            }
        }
    }
    if let Some(n) = m.get("imports") {
        d.imports = imports(n)?;
    }
    if let Some(n) = m.get("overrides") {
        let p = map(n)?;
        d.overrides = imports(required(p, "imports")?)?;
    }
    if let Some(n) = m.get("hide") {
        let p = map(n)?;
        let Node::Sequence(items) = required(p, "imports")? else {
            return Err(error("ENV001", "hide imports requires a list"));
        };
        if items.len() > MAX_IMPORTS {
            return Err(error("ENV007", "too many hidden imports"));
        }
        for n in items {
            if !d.hidden.insert(identifier(n)?) {
                return Err(error("ENV001", "duplicate hidden import"));
            }
        }
    }
    if d.imports
        .keys()
        .any(|k| d.overrides.contains_key(k) || d.hidden.contains(k))
        || d.overrides.keys().any(|k| d.hidden.contains(k))
    {
        return Err(error(
            "ENV004",
            "an alias cannot be added, overridden or hidden more than once",
        ));
    }
    Ok(d)
}
fn imports(node: &Node) -> Result<BTreeMap<String, ImportDefinition>, EnvironmentError> {
    let mut imports = BTreeMap::new();
    for (key, node) in named(node, MAX_IMPORTS)? {
        let m = map(node)?;
        let s = map(required(m, "source")?)?;
        let kind = text(required(s, "kind")?)?;
        if kind == "openapi" && s.contains_key("sha256") {
            return Err(error(
                "ENV001",
                "OpenAPI source.sha256 is not supported; pin the captured environment revision or import a converted Wes descriptor with source.sha256.",
            ));
        }
        let location = match kind {
            "spec" | "openapi" => {
                let (field, value) = match (s.get("file"), s.get("url")) {
                    (Some(v), None) => ("file", v),
                    (None, Some(v)) => ("url", v),
                    _ => return Err(error("ENV001", "spec requires exactly one of file or url")),
                };
                let location = text(value)?;
                if SourceKey::new(kind, location)?.source_field() != field {
                    return Err(error(
                        "ENV001",
                        "spec url requires http:// or https://; use file for local paths",
                    ));
                }
                location
            }
            "process" => text(required(s, "bin")?)?,
            "docker" => text(required(s, "socket")?)?,
            "builtin" => text(required(s, "name")?)?,
            _ => return Err(error("ENV001", "unsupported environment importer kind")),
        };
        let empty = Map::new();
        let (binding_mode, bind) = match m.get("bind") {
            Some(Node::Scalar(Scalar::Text, value))
                if value == "auto" && kind == "builtin" && location == "docker" =>
            {
                (BindingMode::Automatic, &empty)
            }
            Some(bind) => (BindingMode::Explicit, map(bind)?),
            None if kind == "builtin" && location == "docker" => (BindingMode::Unbound, &empty),
            None => {
                return Err(error(
                    "ENV001",
                    "this provider requires bind with an explicit target",
                ));
            }
        };
        let output_policy = match bind.get("output").map(text).transpose()? {
            None | Some("public") => crate::flow::OutputPolicy::Public,
            Some("private") => crate::flow::OutputPolicy::Private,
            Some("confidential-temporary") => crate::flow::OutputPolicy::ConfidentialTemporary,
            Some("confidential") => crate::flow::OutputPolicy::Confidential,
            _ => {
                return Err(error(
                    "ENV001",
                    "output must be public, private, confidential-temporary or confidential",
                ));
            }
        };
        let transport = bind.get("transport").map(text).transpose()?;
        if let Some(transport) = transport {
            if !matches!(transport, "internal" | "curl") {
                return Err(error(
                    "ENV001",
                    "HTTP transport must be internal or curl; custom mappings are not yet supported",
                ));
            }
            if !matches!(kind, "spec" | "openapi") && !(kind == "builtin" && location == "http") {
                return Err(error(
                    "ENV001",
                    "bind.transport is only supported for HTTP providers",
                ));
            }
        }
        let mut auth = BTreeMap::new();
        if let Some(node) = bind.get("auth") {
            if !matches!(kind, "spec" | "openapi") {
                return Err(error(
                    "ENV001",
                    "bind.auth is only supported for spec providers",
                ));
            }
            let choices = map(node)?;
            if choices.len() > 1000 {
                return Err(error("ENV007", "too many authentication choices"));
            }
            for (operation, node) in choices {
                if operation.is_empty()
                    || operation.len() > 4096
                    || operation.split(' ').any(|p| !name(p))
                {
                    return Err(error(
                        "ENV001",
                        "auth choice key must be a space-separated operation path",
                    ));
                }
                let Node::Sequence(names) = node else {
                    return Err(error(
                        "ENV001",
                        "auth choice must be a list of scheme names",
                    ));
                };
                if names.len() > 32 {
                    return Err(error("ENV007", "too many authentication schemes"));
                }
                let mut schemes = std::collections::BTreeSet::new();
                for node in names {
                    let name = text(node)?;
                    if name.is_empty()
                        || name.len() > 256
                        || name.chars().any(char::is_control)
                        || !schemes.insert(name.to_owned())
                    {
                        return Err(error(
                            "ENV001",
                            "invalid or duplicate authentication scheme",
                        ));
                    }
                }
                auth.insert(operation.clone(), schemes.into_iter().collect());
            }
        }
        let endpoint = bind.get("endpoint").map(setting).transpose()?;
        let timeout_ms = bind.get("timeout_ms").map(setting).transpose()?;
        if kind == "process" && endpoint.is_some() {
            return Err(error(
                "ENV001",
                "process imports cannot bind an HTTP endpoint",
            ));
        }
        let mut credentials = BTreeMap::new();
        if let Some(n) = bind.get("credentials") {
            for (key, n) in named(n, MAX_IMPORTS)? {
                let slot = map(n)?;
                credentials.insert(key.clone(), identifier(required(slot, "secret")?)?);
            }
        }
        imports.insert(
            key.clone(),
            ImportDefinition {
                transport: transport.map(str::to_owned),
                auth,
                binding_mode: binding_mode.clone(),
                source_sha256: s
                    .get("sha256")
                    .map(|value| {
                        let hash = text(value)?;
                        if hash.len() != 64
                            || !hash
                                .bytes()
                                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                        {
                            return Err(error(
                                "ENV001",
                                "source sha256 requires 64 lowercase hex characters",
                            ));
                        }
                        Ok(hash.to_owned())
                    })
                    .transpose()?,
                output_policy,
                source: SourceKey::new(kind, location)?,
                target: if binding_mode != BindingMode::Explicit {
                    "local".into()
                } else {
                    identifier(required(bind, "target")?)?
                },
                endpoint,
                timeout_ms,
                credentials,
            },
        );
    }
    Ok(imports)
}
fn setting(node: &Node) -> Result<Setting, EnvironmentError> {
    if let Node::Mapping(m) = node {
        Ok(Setting::Config(identifier(required(m, "config")?)?))
    } else {
        Ok(Setting::Literal(value(node)?))
    }
}
fn value(node: &Node) -> Result<ConfigValue, EnvironmentError> {
    match node {
        Node::Scalar(Scalar::Text, s) => {
            if s.len() > 64 * 1024 {
                return Err(error(
                    "ENV007",
                    "configuration value exceeds its byte budget",
                ));
            }
            Ok(ConfigValue::Text(s.clone()))
        }
        Node::Scalar(Scalar::Int, _) => Ok(ConfigValue::Int(int(node)?)),
        Node::Scalar(Scalar::Bool, _) => Ok(ConfigValue::Bool(boolean(node)?)),
        _ => Err(error(
            "ENV001",
            "configuration values must be Text, Int or Bool",
        )),
    }
}
fn identifier(node: &Node) -> Result<String, EnvironmentError> {
    let s = text(node)?;
    if !name(s) {
        return Err(error("ENV001", "invalid environment field identifier"));
    }
    Ok(s.into())
}
fn named(node: &Node, limit: usize) -> Result<&Map, EnvironmentError> {
    let m = map(node)?;
    if m.len() > limit {
        return Err(error(
            "ENV007",
            "environment declaration count exceeds its budget",
        ));
    }
    if m.keys().any(|key| !name(key)) {
        return Err(error("ENV001", "invalid environment field identifier"));
    }
    Ok(m)
}
fn map(node: &Node) -> Result<&Map, EnvironmentError> {
    if let Node::Mapping(m) = node {
        Ok(m)
    } else {
        Err(error("ENV001", "expected environment mapping"))
    }
}
fn text(node: &Node) -> Result<&str, EnvironmentError> {
    node.text()
        .map_err(|_| error("ENV001", "expected environment text"))
}
fn int(node: &Node) -> Result<i64, EnvironmentError> {
    if let Node::Scalar(Scalar::Int, s) = node {
        s.parse()
            .map_err(|_| error("ENV001", "expected a signed decimal 64-bit integer"))
    } else {
        Err(error("ENV001", "expected an integer"))
    }
}
fn boolean(node: &Node) -> Result<bool, EnvironmentError> {
    if let Node::Scalar(Scalar::Bool, s) = node {
        Ok(s.eq_ignore_ascii_case("true"))
    } else {
        Err(error("ENV001", "expected a boolean"))
    }
}
fn required<'a>(m: &'a Map, key: &str) -> Result<&'a Node, EnvironmentError> {
    m.get(key)
        .ok_or_else(|| error("ENV001", format!("missing environment field: {key}")))
}
