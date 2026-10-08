//! Shared composition of the runnable application for the CLI and the desktop shell.
//! One wiring: adapters, value storage and workspace factory are built here so the
//! two entry points cannot drift apart.
use crate::{ApplicationHandle, ApplicationTask, Config, web};
use std::{
    io,
    num::{NonZeroU64, NonZeroUsize},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use wes_adapters::{
    codec::Limits,
    credentials::{CredentialLimits, MemoryCredentials},
    datasets::{DatasetStore, StoreLimits as DatasetStoreLimits},
    http::HttpConfig,
    imports::{OpenApiImporter, ProcessImporter, SpecImporter},
    inventory::FileInventory,
    journal::Durability,
    process::{ProcessConfig, TerminalHandover, shell},
    storage::TieredValues,
    type_sources::FileTypeSources,
};
use wes_engine::{
    session::SessionStorage,
    storage::{AutoKeep, StoreWorker, StoreWorkerLimits, StoreWorkerTask, spawn_storage},
    workspace::{Workspace, WorkspaceName},
};

type Error = Box<dyn std::error::Error + Send + Sync>;

/// Startup choices shared by every entry point. Defaults mirror the CLI usage text.
pub struct RuntimeOptions {
    /// Private data directory holding workspaces and the tiered value store.
    pub home: PathBuf,
    /// Startup-only, separately provisioned key file. None selects ordinary storage.
    pub storage_key_file: Option<PathBuf>,
    /// Base directory for type sources, importers and shell completion.
    pub base: PathBuf,
    pub workspace: WorkspaceName,
    pub concurrency: NonZeroUsize,
    pub max_streams: NonZeroUsize,
    pub live_budget: Option<NonZeroU64>,
    pub auto_keep: AutoKeep,
    pub node_timeout: Duration,
    /// Batch runs hand the controlling terminal to `@interactive` processes; servers never do.
    pub interactive_terminal: bool,
    /// Override local discovery for isolated embedding/tests; never inherited from workspace data.
    pub docker_candidates: Option<Vec<PathBuf>>,
    /// Explicit injectable secure store for isolated embedding/tests. None uses the OS store.
    pub credential_store: Option<Arc<dyn wes_engine::credentials::material::SecureStore>>,
    /// Explicit vault for isolated embedding/tests, used when no store is injected. None uses
    /// the platform choice: the system store on macOS, otherwise the data home's vault.
    pub credential_vault: Option<Arc<crate::credential_vault::CredentialVault>>,
}

impl RuntimeOptions {
    pub fn new(home: PathBuf, base: PathBuf) -> Self {
        Self {
            home,
            storage_key_file: None,
            base,
            workspace: WorkspaceName::new("default".into()).expect("valid default workspace name"),
            concurrency: NonZeroUsize::new(wes_budgets::get("execution.operations") as usize)
                .expect("positive default concurrency"),
            max_streams: NonZeroUsize::new(wes_budgets::get("execution.streams") as usize).unwrap(),
            live_budget: NonZeroU64::new(wes_budgets::get("storage.live.bytes")),
            auto_keep: AutoKeep::default(),
            node_timeout: Duration::from_millis(wes_budgets::get("execution.node.ms")),
            interactive_terminal: false,
            docker_candidates: None,
            credential_store: None,
            credential_vault: None,
        }
    }
}

/// A running application with its owned storage worker. The embedder decides when to
/// serve, submit or shut down; `shutdown` joins every owner and surfaces failed writes.
pub struct LaunchedRuntime {
    pub handle: ApplicationHandle,
    pub task: ApplicationTask,
    pub worker: StoreWorker,
    pub worker_task: StoreWorkerTask,
    pub credentials: Arc<MemoryCredentials>,
    credential_vault: Option<Arc<crate::credential_vault::CredentialVault>>,
    home: PathBuf,
    base: PathBuf,
    _data_home: crate::data_home::DataHome,
}

pub async fn launch(options: RuntimeOptions) -> Result<LaunchedRuntime, Error> {
    let RuntimeOptions {
        home,
        storage_key_file,
        base,
        workspace,
        concurrency,
        max_streams,
        live_budget,
        auto_keep,
        node_timeout,
        interactive_terminal,
        docker_candidates,
        credential_store,
        credential_vault,
    } = options;
    let requested_home = if home.is_absolute() {
        home
    } else {
        std::env::current_dir()?.join(home)
    };
    let data_home =
        tokio::task::spawn_blocking(move || crate::data_home::DataHome::open(&requested_home))
            .await??;
    let home = data_home.path.clone();
    let home_identity = data_home.identity.id.clone();
    let protection = match storage_key_file {
        Some(path) => {
            let key_home = home.clone();
            let key_identity = home_identity.clone();
            Some(
                tokio::task::spawn_blocking(move || {
                    crate::storage_key::load(&path, &key_home, &key_identity)
                })
                .await??,
            )
        }
        None => None,
    };
    let environment_base = base.clone();
    let documents = Arc::new(crate::api_library::ApiLibrary::new(home.clone()));
    let spec_documents = documents.clone();
    let openapi_compiler = documents.clone();
    let describe_service = documents.describe_service(base.clone());
    let sources = Arc::new(crate::data_home::sources::Sources::new(home.clone()));
    let spec_sources = sources.clone();
    let terminal = interactive_terminal.then(TerminalHandover::new);
    let (shell_description, shell_invoker) = shell(ProcessConfig::default())?;
    let shell_invoker = Arc::new(shell_invoker);
    let shell_conversation = Arc::new(terminal.as_ref().map_or_else(
        || shell_invoker.piped(),
        |terminal| terminal.conversation(&shell_invoker),
    ));
    let (http_description, http_invoker) = wes_adapters::http::direct(HttpConfig::default())?;
    let http_invoker = Arc::new(http_invoker);
    let shell_product = Arc::new(
        wes_engine::imports::ImportProduct::new(shell_description, shell_invoker, vec![])?
            .with_conversations(shell_conversation),
    );
    let http_product = Arc::new(wes_engine::imports::ImportProduct::new(
        http_description,
        http_invoker,
        vec![],
    )?);
    let docker_product = Arc::new(match &docker_candidates {
        Some(paths) => wes_adapters::docker::automatic_with_candidates("docker", paths.clone())?,
        None => wes_adapters::docker::automatic_local("docker")?,
    });
    let directory = home.join("workspaces");
    let live = home.join("values/live");
    let archive = home.join("values/archive");
    let dataset_directory = home.join("datasets");
    let durability = if cfg!(unix) {
        Durability::FileAndDirectory
    } else {
        Durability::File
    };
    let wiring_base = base.clone();
    let (values, datasets, reader, spec, process, credentials) =
        tokio::task::spawn_blocking(move || {
            let credentials = Arc::new(MemoryCredentials::new(CredentialLimits::default()));
            Ok::<_, Error>((
                TieredValues::open_protected(
                    &live,
                    &archive,
                    Limits::default(),
                    durability,
                    live_budget,
                    protection.clone(),
                )?,
                DatasetStore::open_protected(
                    &dataset_directory,
                    durability,
                    DatasetStoreLimits::default(),
                    protection,
                )?,
                Arc::new(FileTypeSources::new(&wiring_base)?.with_archive(spec_sources.clone())),
                Arc::new(
                    SpecImporter::new(&wiring_base, credentials.clone(), HttpConfig::default())?
                        .with_documents(spec_documents)
                        .with_archive(spec_sources),
                ),
                Arc::new({
                    let importer = ProcessImporter::new(&wiring_base, ProcessConfig::default())?;
                    match terminal {
                        Some(terminal) => importer.with_terminal_handover(terminal),
                        None => importer,
                    }
                }),
                credentials,
            ))
        })
        .await??;
    let (worker, worker_task) = spawn_storage(values, datasets, StoreWorkerLimits::default())?;
    let (secure_store, credential_vault) = match (credential_store, credential_vault) {
        (Some(store), _) => (store, None),
        (None, Some(vault)) => (vault.clone() as Arc<_>, Some(vault)),
        (None, None) => crate::credential_store::platform(home.clone()),
    };
    let material = wes_engine::credentials::material::Material::new(secure_store);
    let opened = crate::open(Config {
        directory,
        initial: workspace,
        durability,
        concurrency,
        max_streams,
        type_reader: reader,
        storage: Some(SessionStorage {
            worker: worker.clone(),
            auto_keep,
        }),
        workspace: Arc::new(move || {
            let mut workspace = Workspace::new()
                .with_home_identity(home_identity.clone())
                .with_environment_loader(Arc::new({
                    let loader =
                        wes_adapters::environments::LocalEnvironments::new(&environment_base)
                            .map_err(|e| wes_engine::workspace::WorkspaceError::Rejected {
                                diagnostics: vec![wes_language::Diagnostic::error(
                                    e.code,
                                    wes_language::Span::at(0),
                                    e.message,
                                )],
                                issues: vec![],
                            })?
                            .with_authority(wes_engine::environments::Authority::with_material(
                                material.clone(),
                            ))
                            .with_documents(documents.clone())
                            .with_openapi(documents.clone())
                            .with_archive(sources.clone());
                    match &docker_candidates {
                        Some(paths) => loader.with_docker_candidates(paths.clone()),
                        None => loader,
                    }
                }))
                .with_describe_service(describe_service.clone())
                .with_calculation_services(Arc::new(wes_adapters::codec::CalculationServices))
                .with_default_timeout(node_timeout)?;
            workspace = workspace.with_default_environment(
                "default",
                vec![
                    shell_product.clone(),
                    http_product.clone(),
                    docker_product.clone(),
                ],
            )?;
            workspace.register_importer("spec".into(), spec.clone())?;
            workspace.register_importer(
                "openapi".into(),
                Arc::new(OpenApiImporter::new(spec.clone(), openapi_compiler.clone())),
            )?;
            workspace.register_importer("process".into(), process.clone())?;
            workspace.register_importer(
                "docker".into(),
                Arc::new(wes_adapters::docker::DockerImporter),
            )?;
            Ok(workspace)
        }),
    })
    .await;
    match opened {
        Ok((handle, task)) => Ok(LaunchedRuntime {
            handle,
            task,
            worker,
            worker_task,
            credentials,
            credential_vault,
            home,
            base,
            _data_home: data_home,
        }),
        Err(error) => {
            // The worker is owned here until a runtime exists; failed startup still joins it.
            let _ = worker.shutdown().await;
            let _ = worker_task.join().await;
            Err(error.into())
        }
    }
}

impl LaunchedRuntime {
    pub fn home(&self) -> &std::path::Path {
        &self.home
    }
    pub fn identity(&self) -> &crate::data_home::Identity {
        &self._data_home.identity
    }

    /// Serve the browser client and protocol on `127.0.0.1:port` (0 selects an ephemeral port).
    pub async fn serve(&self, port: u16, site: Option<PathBuf>) -> Result<web::Server, Error> {
        self.serve_client(port, site, false, None, None).await
    }
    /// Desktop UI preferences belong to its data home, independent of the ephemeral HTTP origin.
    pub async fn serve_desktop(&self, site: Option<PathBuf>) -> Result<web::Server, Error> {
        self.serve_client(0, site, true, None, None).await
    }
    pub(crate) async fn serve_managed(
        &self,
        site: Option<PathBuf>,
        connection: crate::data_home::host::Connection,
        terminal_executable: Option<PathBuf>,
    ) -> Result<web::Server, Error> {
        self.serve_client(0, site, true, Some(connection), terminal_executable)
            .await
    }
    async fn serve_client(
        &self,
        port: u16,
        site: Option<PathBuf>,
        desktop: bool,
        connection: Option<crate::data_home::host::Connection>,
        terminal_executable: Option<PathBuf>,
    ) -> Result<web::Server, Error> {
        let home = self.home.clone();
        let base = self.base.clone();
        let credential_vault = self.credential_vault.clone();
        let services = tokio::task::spawn_blocking(move || {
            let preferences =
                desktop.then(|| web::DesktopPreferences::new(home.join("desktop-ui.json")));
            let mut services = browser_services(home, base)?;
            if let Some(executable) = terminal_executable {
                if let Some(terminal) = &mut services.terminal {
                    terminal.executable = executable;
                }
            }
            services.desktop_preferences = preferences;
            if desktop {
                // WebView timers may stop while locked; the native server owns cleanup instead.
                if let Some(terminal) = &mut services.terminal {
                    terminal.idle_timeout = None;
                }
            }
            services.data_home = connection;
            services.credential_vault = credential_vault;
            Ok::<_, Error>(services)
        })
        .await??;
        Ok(web::listen(web::Config {
            services,
            port,
            application: self.handle.clone(),
            values: self.worker.clone(),
            credentials: self.credentials.clone(),
            site,
        })
        .await?)
    }

    /// Cancels the application, then joins every owner. Server shutdown, if any, comes first.
    pub async fn shutdown(self) -> Result<(), Error> {
        if let Some(c) = crate::telemetry::global() {
            c.stop_capture();
        }
        self.handle.shutdown().await;
        let joined = self.task.join().await;
        let drained = self.worker.shutdown().await;
        let worker_joined = self.worker_task.join().await;
        joined?;
        if drained?.failed != 0 {
            return Err(io::Error::other("value storage reported failed operations").into());
        }
        worker_joined?;
        Ok(())
    }
}

/// Read-only browse/completion services shown to the browser client.
pub fn browser_services(home: PathBuf, base: PathBuf) -> Result<web::Services, Error> {
    let mut inventory = FileInventory::new();
    inventory.add(
        "workspaces",
        &home.join("workspaces"),
        "Named workspace journals, recovery streams and retained generations (all workspaces)",
        true,
        1,
    )?;
    inventory.add(
        "archive",
        &home.join("values/archive"),
        "Kept values and archive metadata",
        true,
        0,
    )?;
    inventory.add(
        "live",
        &home.join("values/live"),
        "Evictable values and live-store metadata",
        false,
        0,
    )?;
    let api_library = crate::api_library::ApiLibrary::new(std::fs::canonicalize(&home)?);
    let services = web::Services {
        budgets: crate::budgets::configured_store(),
        terminal: Some(crate::terminal::Config {
            history_home: Some(home.clone()),
            history: None,
            cwd: base.clone(),
            executable: std::env::current_exe()?,
            idle_timeout: Some(Duration::from_secs(120)),
            api_library: Some(api_library.clone()),
        }),
        api_library: Some(api_library),
        inventory: Some(inventory),
        edit_home: Some(home.join("edit")),
        presentations: Some(home.join("presentations")),
        ..Default::default()
    };
    #[cfg(unix)]
    let services = {
        let mut services = services;
        let user_home = std::env::var_os("HOME").map(PathBuf::from);
        let path = std::env::var_os("PATH")
            .map(|paths| std::env::split_paths(&paths).collect())
            .unwrap_or_default();
        services.completers.insert(
            "sh".into(),
            Arc::new(wes_adapters::completion::ShellCompleter::new(
                base, user_home, path,
            )?),
        );
        services
    };
    #[cfg(not(unix))]
    let _ = base;
    Ok(services)
}
