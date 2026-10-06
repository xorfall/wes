//! Where a local Docker daemon listens. Every spelling of that place is read and written here;
//! environment wiring passes the text through and never inspects it.
//!
//! A target's `socket` is one rooted form on every platform: an absolute Unix socket path, or
//! `//./pipe/NAME` for a Windows named pipe. A bound `endpoint` is the same place behind its
//! scheme, `unix://` or `npipe://`. Nothing else is a local endpoint: there is no remote form.

/// The endpoint forms accepted in an environment declaration, for a refusal's explanation.
pub(crate) const DECLARATION_FORMS: &str = "Docker endpoint requires unix:///absolute/path/to/docker.sock, or npipe:////./pipe/NAME on Windows; use bind: auto for local discovery; remote transports are not implemented";
/// The form to offer on this host when discovery finds nothing.
pub(crate) const DECLARATION_EXAMPLE: &str = if cfg!(windows) {
    "npipe:////./pipe/NAME"
} else {
    "unix:///absolute/path/to/docker.sock"
};
const SOCKET_FORMS: &str =
    "Docker requires an explicit absolute Unix socket path, or //./pipe/NAME on Windows";
const PIPE_ROOT: &str = "//./pipe/";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LocalEndpoint {
    /// An absolute Unix socket path.
    Socket(String),
    /// The name of a Windows named pipe, without its namespace.
    Pipe(String),
}
impl LocalEndpoint {
    /// Reads the rooted `socket` form of a target or a captured import.
    pub(crate) fn from_socket(socket: &str) -> Result<Self, &'static str> {
        if !socket.starts_with('/') || socket.len() > 4096 || socket.chars().any(char::is_control) {
            return Err(SOCKET_FORMS);
        }
        match socket.strip_prefix(PIPE_ROOT) {
            None => Ok(Self::Socket(socket.to_owned())),
            Some(name) if !name.is_empty() && !name.contains(['/', '\\']) => {
                Ok(Self::Pipe(name.to_owned()))
            }
            Some(_) => Err(SOCKET_FORMS),
        }
    }
    /// Reads a bound endpoint. The scheme must agree with the place it names.
    pub(crate) fn from_declaration(endpoint: &str) -> Result<Self, &'static str> {
        let read = |scheme: &str| endpoint.strip_prefix(scheme).map(Self::from_socket);
        match (read("unix://"), read("npipe://")) {
            (Some(Ok(socket @ Self::Socket(_))), _) => Ok(socket),
            (_, Some(Ok(pipe @ Self::Pipe(_)))) => Ok(pipe),
            _ => Err(DECLARATION_FORMS),
        }
    }
    /// The rooted `socket` form.
    pub(crate) fn socket(&self) -> String {
        match self {
            Self::Socket(path) => path.clone(),
            Self::Pipe(name) => format!("{PIPE_ROOT}{name}"),
        }
    }
    /// The bound `endpoint` form.
    pub(crate) fn declaration(&self) -> String {
        match self {
            Self::Socket(path) => format!("unix://{path}"),
            Self::Pipe(name) => format!("npipe://{PIPE_ROOT}{name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_place_has_one_socket_form_and_one_declaration_and_they_agree() {
        for (socket, declaration, endpoint) in [
            (
                "/var/run/docker.sock",
                "unix:///var/run/docker.sock",
                LocalEndpoint::Socket("/var/run/docker.sock".into()),
            ),
            (
                "/home/üser/.docker/run/docker.sock",
                "unix:///home/üser/.docker/run/docker.sock",
                LocalEndpoint::Socket("/home/üser/.docker/run/docker.sock".into()),
            ),
            (
                "//./pipe/docker_engine",
                "npipe:////./pipe/docker_engine",
                LocalEndpoint::Pipe("docker_engine".into()),
            ),
        ] {
            assert_eq!(LocalEndpoint::from_socket(socket).unwrap(), endpoint);
            assert_eq!(
                LocalEndpoint::from_declaration(declaration).unwrap(),
                endpoint
            );
            assert_eq!(endpoint.socket(), socket);
            assert_eq!(endpoint.declaration(), declaration);
        }
    }

    #[test]
    fn nothing_but_a_local_place_under_its_own_scheme_is_an_endpoint() {
        let long = format!("/{}", "x".repeat(4096));
        for socket in [
            "",
            "relative/docker.sock",
            "C:/docker.sock",
            r"\\.\pipe\docker_engine",
            "//./pipe/",
            "//./pipe/a/b",
            r"//./pipe/a\b",
            "/var/run/docker\n.sock",
            long.as_str(),
        ] {
            assert!(LocalEndpoint::from_socket(socket).is_err(), "{socket:?}");
        }
        for declaration in [
            "/var/run/docker.sock",
            "unix://relative.sock",
            "unix:////./pipe/docker_engine",
            "npipe:///var/run/docker.sock",
            "npipe:////./pipe/",
            "tcp://127.0.0.1:2375",
            "http://localhost/",
            "ssh://host",
            "unix://",
            "",
        ] {
            assert_eq!(
                LocalEndpoint::from_declaration(declaration),
                Err(DECLARATION_FORMS),
                "{declaration:?}"
            );
        }
    }
}
