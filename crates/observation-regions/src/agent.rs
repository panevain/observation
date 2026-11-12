//! The agent's shared state and its background work.

use std::sync::Arc;
use std::time::Duration;

use observation_upstream::BoundedQueue;

use crate::catalog::Catalog;
use crate::config::Config;
use crate::import::{self, Tools};
use crate::registry::{self, Registry};

/// Queries allowed to wait for a free slot.
const MAX_WAITING_QUERIES: usize = 32;

/// Shared agent state.
#[derive(Debug)]
pub struct Agent {
    pub registry: Arc<Registry>,
    pub catalog: Catalog,
    pub tools: Tools,
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

impl Agent {
    /// Opens the data directory and builds the HTTP client.
    pub fn new(config: &Config) -> Result<Arc<Self>, StartError> {
        // Extracts and the index come from one host; redirects may not leave it,
        // and names may not resolve to private addresses unless allowed.
        let client = observation_upstream::client_builder(
            config.allow_private_upstreams,
            observation_upstream::Redirects::SameHost,
        )
        .user_agent(config.user_agent())
        .connect_timeout(Duration::from_secs(20))
        .build()?;
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
                client.clone(),
                config.catalog_url.clone(),
                config.data_dir.join("catalog.json"),
            ),
            tools: Tools {
                client,
                osmium: config.osmium.clone(),
                overpass_bin: config.overpass_bin.clone(),
            },
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
