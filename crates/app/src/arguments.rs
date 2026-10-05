//! Validate startup choices before opening a data directory. No environment settings are guessed.
use std::{
    collections::HashSet,
    ffi::OsString,
    num::{NonZeroU64, NonZeroUsize},
    path::PathBuf,
    time::Duration,
};
use wes_engine::{runtime::Runtime, storage::AutoKeep, workspace::WorkspaceName};

pub(super) struct Arguments {
    pub json: bool,
    pub sequential: bool,
    pub api_request: Option<PathBuf>,
    pub environment: Option<String>,
    pub environment_revision: Option<wes_core::environments::Revision>,
    pub environment_file: Option<String>,
    pub environment_lock: Option<String>,
    pub activate_environment: bool,
    pub credentials_stdin: bool,
    pub grant_provider: Option<String>,
    pub home: PathBuf,
    pub workspace: WorkspaceName,
    pub command: Option<String>,
    pub file: Option<PathBuf>,
    pub test: Option<PathBuf>,
    pub serve: Option<u16>,
    pub site: Option<PathBuf>,
    pub concurrency: NonZeroUsize,
    pub max_streams: NonZeroUsize,
    pub live_budget: Option<NonZeroU64>,
    pub auto_keep: AutoKeep,
    pub node_timeout: Duration,
}
pub(super) const USAGE: &str = "Usage: wes [--home DIR] [--workspace NAME] --command SOURCE\n       wes [--home DIR] [--workspace NAME] --file FILE\n       wes [--home DIR] [--workspace NAME] --test SCENARIO_YAML\n       wes [--home DIR] [--workspace NAME] --serve PORT [--site DIR]\n       wes [--home DIR] --api-request FILE_OR_DASH\n\nStartup options:\n  --sequential             Execute top-level statements in order in one live session; stop on failure\n  --json                   Print structured values, including help (--command/--file)\n  --home DIR               Data folder (default ~/.wes); shared by named workspaces\n  --api-request FILE_OR_DASH  Library JSON action; '-' reads stdin; excludes command/file/test/serve\n  --test FILE              Run one sequential API scenario; print a policy-checked JSON report\n  --file FILE              Capture one UTF-8 wes script (1 MiB); excludes --command/--test/--serve\n  --env NAME               Explicit environment for batch work\n  --env-revision SHA256    Required revision when selecting installed definitions\n  --env-file FILE          Apply this reviewed package before any command; drift refuses execution\n  --env-lock FILE          Install captured definitions into an empty environment registry\n  --activate-env           Explicitly reopen selected environment execution after load\n  --credentials-stdin      Read a bounded JSON reference/value map from stdin (never source/argv)\n  --grant-provider ALIAS   Grant selected environment/revision credentials for 5 minutes\n  --concurrency COUNT       Concurrent entered operations, 1..1024 (default 4)\n  --max-streams COUNT       Reserved stream slots across all workspaces, 1..1024 (default 32)\n  --live-budget BYTES       Positive live-result budget or unlimited (default 1073741824)\n  --keep-under BYTES        Keep finite results at or below BYTES of encoded storage, including type/provenance (default 10485760)\n  --no-auto-keep            Disable automatic finite-result retention; excludes --keep-under\n  --node-timeout SECONDS    Positive default finite node timeout (default 900)\n\nRuns source or serves the browser client on 127.0.0.1.\nBuild gui first and pass --site gui/dist to serve its assets.\nUse --sequential for workflows combining declarations and workspace lifecycle operations.\nUse --serve for ongoing streams; --command/--file capture the window available after opening.\nIn batch mode, @interactive processes inherit this client's input/output; captured output stays empty.\nScript-relative input paths use the script directory. CLI paths keep the launch directory as base.\nIn --serve mode, @interactive processes use browser input and bounded live output.\nThe default timeout does not limit open streams or interactive conversations; explicit node timeouts do.\nConcurrency and stream capacity are shared across all workspaces.\nOperations hold concurrency while executing; opening streams release it once open.\nInteractive conversations retain concurrency; stream slots remain reserved through cleanup.\n:env disable affects this live session, not saved definitions.\nFor installed-registry reads, omit --env-file; use --env NAME --env-revision REVISION to select without applying a package.\nOther options configure each opened workspace; changing retention does not archive earlier results.";

pub(super) fn arguments() -> Result<Option<Arguments>, String> {
    parse(std::env::args_os().skip(1))
}
fn number(value: OsString, message: &str) -> Result<u64, String> {
    let text = value.to_str().ok_or_else(|| message.to_owned())?;
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(message.into());
    }
    text.parse().map_err(|_| message.into())
}
fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Option<Arguments>, String> {
    let mut args = args.into_iter();
    let mut seen = HashSet::new();
    let (mut home, mut workspace, mut command, mut serve, mut site) =
        (None, None, None, None, None);
    let mut max_streams =
        NonZeroUsize::new(wes_budgets::get("execution.streams") as usize).unwrap();
    let mut concurrency =
        NonZeroUsize::new(wes_budgets::get("execution.operations") as usize).unwrap();
    let mut live_budget = NonZeroU64::new(wes_budgets::get("storage.live.bytes"));
    let mut under = wes_budgets::get("storage.keep.bytes");
    let mut automatic = true;
    let mut node_timeout = Duration::from_millis(wes_budgets::get("execution.node.ms"));
    let (mut environment, mut environment_revision, mut environment_file, mut grant_provider) =
        (None, None, None, None);
    let (mut activate_environment, mut credentials_stdin) = (false, false);
    let mut environment_lock = None;
    let mut file = None;
    let mut test = None;
    let mut api_request = None;
    let mut json = false;
    let mut sequential = false;
    while let Some(flag) = args.next() {
        let flag = flag.to_str().ok_or("argument flags must be UTF-8")?;
        if flag == "--help" || flag == "-h" {
            return Ok(None);
        }
        if !seen.insert(flag.to_owned()) {
            return Err(format!("repeated argument: {flag}"));
        }
        if flag == "--sequential" {
            sequential = true;
            continue;
        }
        if flag == "--json" {
            json = true;
            continue;
        }
        if flag == "--no-auto-keep" {
            automatic = false;
            continue;
        }
        if flag == "--activate-env" {
            activate_environment = true;
            continue;
        }
        if flag == "--credentials-stdin" {
            credentials_stdin = true;
            continue;
        }
        if !matches!(
            flag,
            "--home"
                | "--api-request"
                | "--env"
                | "--env-revision"
                | "--env-file"
                | "--env-lock"
                | "--grant-provider"
                | "--workspace"
                | "--command"
                | "--file"
                | "--test"
                | "--serve"
                | "--site"
                | "--max-streams"
                | "--concurrency"
                | "--live-budget"
                | "--keep-under"
                | "--node-timeout"
        ) {
            return Err(format!("unknown argument: {flag}"));
        }
        let value = args
            .next()
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag {
            "--api-request" => api_request = Some(PathBuf::from(value)),
            "--env" => {
                environment = Some(
                    value
                        .into_string()
                        .map_err(|_| "environment must be UTF-8")?,
                )
            }
            "--env-revision" => {
                environment_revision = Some(
                    value
                        .to_str()
                        .ok_or("revision must be UTF-8")?
                        .parse()
                        .map_err(|_| "invalid environment revision")?,
                )
            }
            "--env-file" => {
                environment_file = Some(
                    value
                        .into_string()
                        .map_err(|_| "definition path must be UTF-8")?,
                )
            }
            "--env-lock" => {
                environment_lock = Some(value.into_string().map_err(|_| "lock path must be UTF-8")?)
            }
            "--grant-provider" => {
                grant_provider = Some(
                    value
                        .into_string()
                        .map_err(|_| "provider alias must be UTF-8")?,
                )
            }
            "--home" => home = Some(PathBuf::from(value)),
            "--workspace" => {
                workspace = Some(
                    value
                        .into_string()
                        .map_err(|_| "workspace name must be UTF-8")?,
                )
            }
            "--command" => command = Some(value.into_string().map_err(|_| "source must be UTF-8")?),
            "--file" => file = Some(PathBuf::from(value)),
            "--test" => test = Some(PathBuf::from(value)),
            "--serve" => {
                serve = Some(
                    u16::try_from(number(value, "--serve requires a port from 0 to 65535")?)
                        .map_err(|_| "--serve requires a port from 0 to 65535")?,
                )
            }
            "--site" => site = Some(PathBuf::from(value)),
            "--max-streams" => {
                let count = number(value, "--max-streams requires a count from 1 to 1024")?;
                if !(1..=wes_engine::driver::MAX_STREAMS as u64).contains(&count) {
                    return Err("--max-streams requires a count from 1 to 1024".into());
                }
                max_streams = NonZeroUsize::new(count as usize).unwrap();
            }
            "--concurrency" => {
                let count = number(value, "--concurrency requires a count from 1 to 1024")?;
                if !(1..=1024).contains(&count) {
                    return Err("--concurrency requires a count from 1 to 1024".into());
                }
                concurrency = NonZeroUsize::new(count as usize).unwrap();
            }
            "--live-budget" => {
                live_budget = if value == "unlimited" {
                    None
                } else {
                    Some(
                        NonZeroU64::new(number(
                            value,
                            "--live-budget requires positive bytes or unlimited",
                        )?)
                        .ok_or("--live-budget requires positive bytes or unlimited")?,
                    )
                }
            }
            "--keep-under" => {
                under = number(value, "--keep-under requires a nonnegative byte count")?
            }
            "--node-timeout" => {
                node_timeout =
                    Duration::from_secs(number(value, "--node-timeout requires positive seconds")?);
                Runtime::<()>::new()
                    .set_default_timeout(node_timeout)
                    .map_err(|_| "--node-timeout exceeds the positive runtime duration range")?;
            }
            _ => unreachable!("validated flag"),
        }
    }
    if usize::from(command.is_some())
        + usize::from(file.is_some())
        + usize::from(test.is_some())
        + usize::from(serve.is_some())
        + usize::from(api_request.is_some())
        != 1
    {
        return Err(
            "exactly one of --command, --file, --test, --serve or --api-request is required".into(),
        );
    }
    if (environment.is_some()
        || environment_file.is_some()
        || environment_lock.is_some()
        || environment_revision.is_some()
        || activate_environment
        || credentials_stdin
        || grant_provider.is_some())
        && command.is_none()
        && file.is_none()
        && test.is_none()
    {
        return Err("environment/credential startup flags require --command, --file or --test; browser panes select independently".into());
    }
    if environment_file.is_some() && environment_lock.is_some() {
        return Err("--env-file and --env-lock are mutually exclusive".into());
    }
    if environment.is_some()
        && environment_revision.is_none()
        && environment_file.is_none()
        && environment_lock.is_none()
    {
        return Err(
            "--env requires --env-revision REVISION for installed definitions, or an explicitly applied --env-file/--env-lock; inspect revisions with :list environments or :inspect env:\"NAME\" without --env-file".into(),
        );
    }
    if environment.is_none()
        && (environment_revision.is_some() || activate_environment || grant_provider.is_some())
    {
        return Err("revision, activation and grant flags require --env".into());
    }
    if credentials_stdin && command.as_ref().is_some_and(|s| s.contains("@interactive")) {
        return Err("credential stdin cannot share a command with interactive input".into());
    }
    if sequential && command.is_none() && file.is_none() {
        return Err("--sequential requires --command or --file".into());
    }
    if json && command.is_none() && file.is_none() {
        return Err("--json requires --command or --file".into());
    }
    if site.is_some() && serve.is_none() {
        return Err("--site requires --serve".into());
    }
    if !automatic && seen.contains("--keep-under") {
        return Err("--no-auto-keep cannot be combined with --keep-under".into());
    }
    Ok(Some(Arguments {
        json,
        sequential,
        api_request,
        environment,
        environment_revision,
        environment_file,
        environment_lock,
        activate_environment,
        credentials_stdin,
        grant_provider,
        home: home
            .or_else(|| std::env::home_dir().map(|h| wes::data_home::default_home(&h)))
            .ok_or("Cannot determine home directory; pass --home DIR")?,
        workspace: WorkspaceName::new(workspace.unwrap_or_else(|| "default".into()))
            .map_err(|e| e.to_string())?,
        command,
        file,
        test,
        serve,
        site,
        concurrency,
        max_streams,
        live_budget,
        auto_keep: if automatic {
            AutoKeep::UpToBytes(under)
        } else {
            AutoKeep::Never
        },
        node_timeout,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parsed(extra: &[&str]) -> Result<Option<Arguments>, String> {
        parse(
            ["--home", "/never-opened", "--command", ":help"]
                .into_iter()
                .chain(extra.iter().copied())
                .map(OsString::from),
        )
    }
    #[test]
    fn scenario_mode_accepts_environment_options_and_rejects_other_entry_points() {
        let base = ["--home", "/never-opened", "--test", "suite.yaml"];
        let accepted = parse(
            base.into_iter()
                .chain([
                    "--env-file",
                    "environments.yaml",
                    "--env",
                    "demo",
                    "--activate-env",
                ])
                .map(OsString::from),
        )
        .unwrap()
        .unwrap();
        assert_eq!(accepted.test, Some(PathBuf::from("suite.yaml")));
        for extra in [
            vec!["--command", ":help"],
            vec!["--file", "x.wes"],
            vec!["--serve", "0"],
            vec!["--api-request", "x.json"],
            vec!["--test", "other.yaml"],
        ] {
            assert!(parse(base.into_iter().chain(extra).map(OsString::from)).is_err());
        }
    }
    #[test]
    fn startup_defaults_and_explicit_choices_are_typed_without_io() {
        let default = parsed(&[]).unwrap().unwrap();
        assert_eq!(default.concurrency.get(), 4);
        assert_eq!(default.max_streams.get(), 32);
        assert_eq!(default.live_budget.unwrap().get(), 1024 * 1024 * 1024);
        assert_eq!(default.auto_keep, AutoKeep::default());
        assert_eq!(
            default.node_timeout,
            Duration::from_millis(wes_budgets::get("execution.node.ms"))
        );
        let chosen = parsed(&[
            "--max-streams",
            "7",
            "--concurrency",
            "2",
            "--live-budget",
            "unlimited",
            "--keep-under",
            "0",
            "--node-timeout",
            "3",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(chosen.concurrency.get(), 2);
        assert_eq!(chosen.max_streams.get(), 7);
        assert_eq!(chosen.live_budget, None);
        assert_eq!(chosen.auto_keep, AutoKeep::UpToBytes(0));
        assert_eq!(chosen.node_timeout, Duration::from_secs(3));
        assert_eq!(
            parsed(&["--no-auto-keep"]).unwrap().unwrap().auto_keep,
            AutoKeep::Never
        );
    }
    #[test]
    fn invalid_ambiguous_and_repeated_startup_choices_are_refused_before_io() {
        for extra in [
            vec!["--concurrency", "0"],
            vec!["--concurrency", "1025"],
            vec!["--live-budget", "0"],
            vec!["--keep-under", "-1"],
            vec!["--node-timeout", "0"],
            vec!["--node-timeout", "18446744073709551615"],
            vec!["--concurrency", "2", "--concurrency", "3"],
            vec!["--no-auto-keep", "--keep-under", "10"],
            vec!["--live-budget", "+1"],
            vec!["--keep-under", "18446744073709551616"],
            vec!["--no-auto-keep", "--no-auto-keep"],
        ] {
            assert!(parsed(&extra).is_err(), "{extra:?}");
        }
    }
}
