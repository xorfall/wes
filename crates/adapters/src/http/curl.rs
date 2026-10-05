//! Curl receives structured request material via stdin, never remote argv/exec metadata.
//! Private temporary request files have mode 0600 inside a mode-0700 directory.
use super::{Failure, HttpConfig, transport::Response};
use crate::execution_targets::finite;
use reqwest::{
    Method, Request,
    header::{HeaderMap, HeaderName, HeaderValue, LOCATION},
};
use std::time::Instant;
use wes_core::environments::Binding;
use wes_engine::{driver::CancellationToken, environments::Authority, providers::InvocationError};

const SCRIPT: &str = r#"set -eu
umask 077
for tool in curl mktemp dd cat rm; do command -v "$tool" >/dev/null 2>&1 || exit 127; done
unset CURL_HOME CURL_CA_BUNDLE SSL_CERT_FILE SSL_CERT_DIR SSLKEYLOGFILE
dir=$(mktemp -d /tmp/wes-http.XXXXXXXXXX) || exit 126
trap 'rm -rf "$dir"' EXIT
trap 'exit 125' HUP INT TERM
IFS= read -r count || exit 126
case "$count" in ''|*[!0-9]*) exit 126;; esac
dd bs=1 count="$count" of="$dir/config" 2>/dev/null || exit 126
cat > "$dir/body" || exit 126
# Some SSH servers attach stderr to a socket: /dev/stderr cannot be opened there.
# Limit the header file before curl opens it; stdout remains a streamed byte channel.
rc=0
(
  ulimit -f "$2" || exit 126
  if [ "$1" = body ]; then
    curl --disable --silent --config "$dir/config" --dump-header "$dir/headers" --output - --data-binary "@$dir/body"
  else
    curl --disable --silent --config "$dir/config" --dump-header "$dir/headers" --output -
  fi
) || rc=$?
if [ -f "$dir/headers" ]; then cat "$dir/headers" >&2 || exit 126; fi
exit "$rc"
"#;
fn quoted(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{b}' => out.push_str("\\v"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
fn encode(request: &Request, config: HttpConfig) -> Result<Vec<u8>, InvocationError> {
    let mut options = format!(
        "globoff\nhttp1.1\nproxy = \"\"\nnoproxy = \"*\"\nproto = \"=http,https\"\nmax-redirs = 0\nconnect-timeout = {}\nmax-time = {}\nmax-filesize = {}\nurl = {}\nrequest = {}\n",
        config.connect_timeout.as_secs_f64(),
        config.request_timeout.as_secs_f64(),
        config.response.bytes,
        quoted(request.url().as_str()),
        quoted(request.method().as_str())
    );
    if request.method() == Method::HEAD {
        options.push_str("head\n");
    }
    for name in ["accept", "user-agent", "content-type", "expect"] {
        if !request.headers().contains_key(name) {
            options.push_str(&format!("header = {}\n", quoted(&format!("{name}:"))));
        }
    }
    for (name, value) in request.headers() {
        let value = value
            .to_str()
            .map_err(|_| Failure::Request("curl requires text request headers").error())?;
        let header = if value.is_empty() {
            format!("{name};")
        } else {
            format!("{name}: {value}")
        };
        options.push_str(&format!("header = {}\n", quoted(&header)));
    }
    let mut input = format!("{}\n", options.len()).into_bytes();
    input.extend_from_slice(options.as_bytes());
    if let Some(body) = request.body() {
        let bytes = body
            .as_bytes()
            .ok_or_else(|| Failure::Request("curl requires a finite byte body").error())?;
        if bytes.len() > config.request.bytes {
            return Err(Failure::Size.error());
        }
        input.extend_from_slice(bytes);
    }
    Ok(input)
}
fn parse(headers: &[u8], body: Vec<u8>, config: HttpConfig) -> Result<Response, InvocationError> {
    if headers.len() > config.header_bytes || body.len() > config.response.bytes {
        return Err(Failure::Size.error());
    }
    let mut remaining = headers;
    let mut final_head = None;
    while !remaining.is_empty() {
        let end = remaining
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or_else(|| Failure::Response.error())?;
        let mut lines = remaining[..end]
            .split(|b| *b == b'\n')
            .map(|line| line.strip_suffix(b"\r").unwrap_or(line));
        let status_line =
            std::str::from_utf8(lines.next().ok_or_else(|| Failure::Response.error())?)
                .map_err(|_| Failure::Response.error())?;
        let mut parts = status_line.split_whitespace();
        let version = parts
            .next()
            .filter(|v| matches!(*v, "HTTP/1.0" | "HTTP/1.1" | "HTTP/2" | "HTTP/3"))
            .ok_or_else(|| Failure::Response.error())?
            .to_owned();
        let status = parts
            .next()
            .and_then(|s| s.parse::<u16>().ok())
            .filter(|s| (100..600).contains(s))
            .ok_or_else(|| Failure::Response.error())?;
        if status == 101 || final_head.is_some() {
            return Err(Failure::Response.error());
        }
        let mut parsed = HeaderMap::new();
        for line in lines {
            let colon = line
                .iter()
                .position(|b| *b == b':')
                .ok_or_else(|| Failure::Response.error())?;
            let name =
                HeaderName::from_bytes(&line[..colon]).map_err(|_| Failure::Response.error())?;
            let value = line[colon + 1..].trim_ascii();
            parsed.append(
                name,
                HeaderValue::from_bytes(value).map_err(|_| Failure::Response.error())?,
            );
        }
        if status >= 200 {
            final_head = Some((status, version, parsed));
        }
        remaining = &remaining[end + 4..];
        // curl appends HTTP trailers to the header channel. They are not response headers.
        if final_head.is_some() && !remaining.is_empty() && !remaining.starts_with(b"HTTP/") {
            break;
        }
    }
    let (status, version, headers) = final_head.ok_or_else(|| Failure::Response.error())?;
    Ok(Response {
        status,
        version,
        headers,
        body,
    })
}
pub(super) async fn send(
    binding: &Binding,
    authority: &Authority,
    mut request: Request,
    config: HttpConfig,
    token: CancellationToken,
) -> Result<Response, InvocationError> {
    let start = Instant::now();
    let origin = request.url().origin();
    for hop in 0..10 {
        let mut current = config;
        current.request_timeout = config
            .request_timeout
            .checked_sub(start.elapsed())
            .filter(|t| !t.is_zero())
            .ok_or_else(|| Failure::Timeout.error())?;
        let input = encode(&request, current)?;
        let output = finite::execute(
            binding.clone(),
            authority.clone(),
            vec![
                "/bin/sh".into(),
                "-c".into(),
                SCRIPT.into(),
                "wes-http".into(),
                if request.body().is_some() {
                    "body"
                } else {
                    "empty"
                }
                .into(),
                config.header_bytes.div_ceil(512).max(1).to_string(),
            ],
            input,
            current.request_timeout,
            config.response.bytes.saturating_add(config.header_bytes),
            token.clone(),
        )
        .await?;
        match output.code {
            0 => (),
            127 => return Err(Failure::Request("curl transport requires curl and POSIX tools (mktemp, dd, cat, rm) on the selected target; install the missing tool there").error()),
            125 | 126 => return Err(Failure::Request("curl transport could not prepare private request input on the selected target").error()),
            2 | 4 => return Err(Failure::Request("Target curl lacks required HTTP options or protocol support; install a current curl on that target").error()),
            28 => return Err(Failure::Timeout.error()),
            63 | 153 => return Err(Failure::Size.error()),
            _ => return Err(Failure::Transport.error()),
        }
        let body = if request.method() == Method::HEAD {
            vec![]
        } else {
            output.stdout
        };
        let response = parse(&output.stderr, body, config)?;
        if !matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            return Ok(response);
        }
        let Some(location) = response.headers.get(LOCATION).and_then(|v| v.to_str().ok()) else {
            return Ok(response);
        };
        let next = request
            .url()
            .join(location)
            .map_err(|_| Failure::Redirect.error())?;
        if next.as_str().len() > config.url_bytes
            || hop == 9
            || next.origin() != origin
            || !next.username().is_empty()
            || next.password().is_some()
        {
            return Err(Failure::Redirect.error());
        }
        if (matches!(response.status, 301 | 302) && request.method() == Method::POST)
            || (response.status == 303 && request.method() != Method::HEAD)
        {
            *request.method_mut() = Method::GET;
            *request.body_mut() = None;
            for header in [
                "content-type",
                "content-length",
                "transfer-encoding",
                "content-encoding",
            ] {
                request.headers_mut().remove(header);
            }
        }
        *request.url_mut() = next;
    }
    Err(Failure::Redirect.error())
}

#[cfg(all(test, unix))]
mod tests;
