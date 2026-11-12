//! The agent's shared state and its background work.

use std::sync::Arc;
use std::time::Duration;

use observation_upstream::{BoundedQueue, GuardedHttp, Redirects};

use crate::catalog::Catalog;
use crate::config::Config;
use crate::import::{self, Tools};
use crate::ports::{Download, ExtractDownload, QueryRunner, Toolchain};
use crate::query::Osm3sQuery;
use crate::registry::{self, Registry};
use crate::tools::OsmTools;

/// Queries allowed to wait for a free slot.
const MAX_WAITING_QUERIES: usize = 32;

/// Shared agent state.
#[derive(Debug)]
pub struct Agent {
    pub registry: Arc<Registry>,
    pub catalog: Catalog,
    pub tools: Tools,
    pub(crate) query_runner: Arc<dyn QueryRunner>,
    pub(crate) token: Option<String>,
    pub(crate) queries: BoundedQueue,
    pub(crate) query_timeout: Duration,
    /// Flipped when shutting down, to end long polls.
    pub(crate) shutdown: tokio::sync::watch::Sender<bool>,
}

/// The agent could not start.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    State(#[from] registry::OpenError),
    #[error("could not build the HTTP client: {0}")]
    Client(#[from] reqwest::Error),
}

/// What the agent talks to: see [`Agent::from_parts`].
#[derive(Debug, Clone)]
pub struct Ports {
    /// For the extract list and the boundaries. Temporary: they move to the
    /// HTTP port next, and reqwest leaves this API.
    pub client: reqwest::Client,
    pub download: Arc<dyn Download>,
    pub toolchain: Arc<dyn Toolchain>,
    pub query_runner: Arc<dyn QueryRunner>,
}

impl Ports {
    /// The download server and the programs that `config` names.
    pub fn production(config: &Config) -> Result<Self, reqwest::Error> {
        // Extracts and the index come from one host; redirects may not leave it,
        // and names may not resolve to private addresses unless allowed.
        let client = observation_upstream::client_builder(
            config.allow_private_upstreams,
            observation_upstream::Redirects::SameHost,
        )
        .user_agent(config.user_agent())
        .connect_timeout(Duration::from_secs(20))
        .build()?;
        let download = GuardedHttp::builder(config.allow_private_upstreams, Redirects::SameHost)
            .user_agent(&config.user_agent())
            .connect_timeout(Duration::from_secs(20))
            .build()?;
        Ok(Self {
            client,
            download: Arc::new(ExtractDownload(download)),
            toolchain: Arc::new(OsmTools {
                osmium: config.osmium.clone(),
                overpass_bin: config.overpass_bin.clone(),
            }),
            query_runner: Arc::new(Osm3sQuery {
                bin: config.overpass_bin.clone(),
            }),
        })
    }
}

impl Agent {
    /// Opens the data directory and builds the production ports.
    pub fn new(config: &Config) -> Result<Arc<Self>, StartError> {
        Self::from_parts(config, Ports::production(config)?)
    }

    /// Opens the data directory; the agent talks to the world through `ports`.
    pub fn from_parts(config: &Config, ports: Ports) -> Result<Arc<Self>, StartError> {
        let registry = Arc::new(
            Registry::open(
                &config.data_dir,
                config.max_download_bytes,
                config.refresh_days,
            )?
            .with_refresh_range(config.min_refresh_days, config.max_refresh_days),
        );
        Ok(Arc::new(Self {
            catalog: Catalog::new(
                ports.client.clone(),
                config.catalog_url.clone(),
                config.data_dir.join("catalog.json"),
            ),
            tools: Tools {
                client: ports.client,
                download: ports.download,
                toolchain: ports.toolchain,
            },
            query_runner: ports.query_runner,
            registry,
            token: config.token.clone(),
            queries: BoundedQueue::new(config.query_concurrency, MAX_WAITING_QUERIES),
            query_timeout: config.query_timeout,
            shutdown: tokio::sync::watch::Sender::new(false),
        }))
    }

    /// Ends pending long polls so a graceful shutdown does not wait on them.
    pub fn begin_shutdown(&self) {
        self.shutdown.send_replace(true);
    }

    /// Runs queued imports one at a time, forever.
    pub async fn run_worker(self: Arc<Self>) {
        loop {
            let (id, job) = self.registry.next_job().await;
            let this = Arc::clone(&self);
            let region = id.clone();
            let task = tokio::spawn(async move {
                let extract = match this.catalog.find(&region).await {
                    Ok(Some(x)) => x,
                    Ok(None) => {
                        return Err(format!(
                            "`{region}` is no longer offered by the extract list"
                        ));
                    }
                    Err(e) => return Err(e.to_string()),
                };
                import::run(&this.tools, &this.registry, job, &extract).await
            });
            if !self.registry.started(&id, job, task.abort_handle()) {
                let _ = task.await;
                self.registry.finish(&id, job, None);
                continue;
            }
            tracing::info!(region = %id, "import started");
            let result = match task.await {
                Ok(r) => Some(r),
                Err(e) if e.is_cancelled() => {
                    tracing::info!(region = %id, "import cancelled");
                    None
                }
                Err(e) => Some(Err(format!("the import crashed: {e}"))),
            };
            // Cancelled and failed imports leave their downloads behind.
            let work = self
                .registry
                .data_dir()
                .join("work")
                .join(registry::slug(&id));
            let _ = tokio::fs::remove_dir_all(work).await;
            self.registry.finish(&id, job, result);
        }
    }

    /// Queues re-imports of regions older than the refresh interval, hourly.
    pub async fn run_refresher(self: Arc<Self>) {
        let mut tick = tokio::time::interval(Duration::from_secs(3600));
        loop {
            tick.tick().await;
            for (id, name) in self.registry.due_for_refresh() {
                tracing::info!(region = %id, "queueing scheduled refresh");
                let _ = self.registry.enqueue(&id, &name);
            }
        }
    }
}

#[cfg(test)]
mod tests;
