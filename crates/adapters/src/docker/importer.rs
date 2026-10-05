//! Offline discovery and captured socket imports. Environment binding owns live authority.
use super::observation;
use std::sync::Arc;
use wes_core::{Data, Primitive, Shape, capability::Parameter, environments::Binding};
use wes_engine::{
    driver::CancellationToken,
    environments::Authority,
    imports::{
        EnvironmentFactory, ImportError, ImportMode, ImportProduct, ImportRecipe, ImportRequest,
        ImportSnapshot, Importer,
    },
    providers::{Call, InvocationFuture, Invoker},
    streams::{StreamFuture, StreamSink, StreamingInvoker},
};

struct Unconnected;
fn connection_required() -> wes_engine::providers::InvocationError {
    observation::failure(
        "DOCKER_CONNECTION",
        "Docker is available but has no connection. Connect explicitly with :import docker socket:\"/absolute/path/to/docker.sock\" as:docker (agents can use a new alias). No daemon was contacted.",
    )
}
impl Invoker for Unconnected {
    fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
        Box::pin(async { Err(connection_required()) })
    }
}
impl StreamingInvoker for Unconnected {
    fn subscribe(&self, _: Call, _: StreamSink, _: CancellationToken) -> StreamFuture {
        Box::pin(async { Err(connection_required()) })
    }
}
pub fn unconnected(alias: &str) -> Result<ImportProduct, ImportError> {
    ImportProduct::new(
        observation::description(alias).map_err(ImportError::Input)?,
        Arc::new(Unconnected),
        vec![],
    )
    .map(|product| product.with_streams(Arc::new(Unconnected)))
}

pub struct DockerImporter;
fn socket(request: &ImportRequest) -> Result<&str, ImportError> {
    if request.kind() != "docker" || request.arguments().len() != 1 {
        return Err(ImportError::Input(
            "Docker import requires only socket: and optional as:",
        ));
    }
    match request.arguments().get("socket").map(|v| v.data()) {
        Some(Data::Text(socket)) if valid_socket(socket) => Ok(socket),
        _ => Err(ImportError::Input(
            "Docker socket must be an explicit absolute Unix socket path",
        )),
    }
}
fn valid_socket(socket: &str) -> bool {
    socket.starts_with('/') && socket.len() <= 4096 && !socket.chars().any(char::is_control)
}
impl Importer for DockerImporter {
    fn parameters(&self) -> Vec<Parameter> {
        vec![Parameter::new(
            "socket",
            Shape::Primitive(Primitive::Text),
            true,
        )]
    }
    fn capture(
        &self,
        request: &ImportRequest,
        max_bytes: usize,
    ) -> Result<ImportRecipe, ImportError> {
        let socket = socket(request)?;
        if socket.len() > max_bytes {
            return Err(ImportError::Capacity);
        }
        ImportRecipe::new("docker/socket/v1".into(), socket.into())
    }
    fn build(
        &self,
        snapshot: &ImportSnapshot,
        _: ImportMode,
    ) -> Result<ImportProduct, ImportError> {
        let socket = socket(snapshot.request())?;
        if snapshot.recipe().format() != "docker/socket/v1" || snapshot.recipe().source() != socket
        {
            return Err(ImportError::InvalidRecipe);
        }
        let alias = snapshot.request().alias().unwrap_or("docker");
        Ok(
            unconnected(alias)?.with_environment_factory(Arc::new(SocketBinding {
                alias: alias.into(),
                socket: socket.into(),
            })),
        )
    }
}
struct SocketBinding {
    alias: String,
    socket: String,
}
impl EnvironmentFactory for SocketBinding {
    fn bind(&self, binding: &Binding, authority: Authority) -> Result<ImportProduct, ImportError> {
        observation::build_at(&self.alias, &self.socket, binding, authority)
            .map_err(ImportError::Input)
    }
}
