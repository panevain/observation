//! The MCP server over Streamable HTTP, behind bearer tokens.
//!
//! `POST /mcp` takes MCP messages. It is stateless in MCP's sense: there are
//! no `Mcp-Session-Id` sessions, each request is served on its own and
//! answered with JSON (or with an SSE stream for `subscriptions/listen`).
//! `GET /healthz` answers `ok` without authentication.
//!
//! Every `/mcp` request must carry `Authorization: Bearer <secret>` for one
//! of the configured tokens (see [`config`]), unless authentication was
//! turned off. Each token has its own [`ObservationMcp`], and so its own
//! connection to the user's app ([`crate::link`]): a client that pairs with
//! the app keeps that pairing across its requests, and no other client can
//! use it. Without authentication every request shares one.
//!
//! rmcp checks `Host` against the allowed hosts and any `Origin` against the
//! allowed origins (none by default, so browsers are refused), which guards
//! against DNS rebinding. For an allowed origin the server also speaks CORS,
//! so a chat page in a browser (llama.cpp's, say) can call it: preflights are
//! answered without a token, and responses, 401s included, carry
//! `Access-Control-Allow-Origin` for that origin. Bodies are capped at [`MAX_BODY_BYTES`]. A request
//! must produce its first message within [`REQUEST_TIMEOUT`] or it is
//! cancelled and answered with 503; an ordinary request's first message is
//! its result, while `subscriptions/listen` acknowledges at once and then
//! streams for as long as the client stays.
//!
//! The server speaks plain HTTP. Bind it to loopback, or put it behind a
//! reverse proxy that terminates TLS: tokens sent over plain HTTP on a
//! network can be read by anyone on the path.

pub mod auth;
pub mod config;

use crate::ObservationMcp;
use crate::toolsets::{ToolSelection, Toolbox};
use auth::Tokens;
use axum::Router;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::http::{HeaderName, HeaderValue, Method, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use config::{Auth, HttpConfig, origin_allowed};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use tower_http::cors::{AllowHeaders, AllowOrigin, CorsLayer};
use tower_http::timeout::TimeoutLayer;
use tracing::Instrument;

/// The largest request body accepted. A scene file at its limits (256
/// cameras, 64 custom models) is well under this.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

/// How long a request may take to produce its first message (for an
/// ordinary request, its result) before it is cancelled and answered with
/// 503. A tool on a connected app's real area may first wait for the app's
/// server to build the area's bundle ([`crate::link`] allows that 90 s), so
/// this leaves half a minute more for the tool itself.
pub const REQUEST_TIMEOUT: Duration =
    Duration::from_secs(crate::link::BUNDLE_TIMEOUT.as_secs() + 30);

/// How long open streams may keep the server up after SIGINT or SIGTERM.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// How long a browser may cache a preflight's answer.
pub const CORS_MAX_AGE: Duration = Duration::from_secs(600);

/// The endpoint's path.
pub const MCP_PATH: &str = "/mcp";

type McpService = StreamableHttpService<ObservationMcp, NeverSessionManager>;

/// Who may use the endpoint, and the server each of them reaches.
enum Clients {
    /// The tokens, each with its own server.
    Tokens(Tokens<McpService>),
    /// Anyone, all through one server.
    Anyone(McpService),
}

/// The HTTP application, offering every tool. Cancelling `shutdown` stops
/// the MCP services accepting requests and ends their streams.
pub fn router(config: &HttpConfig, shutdown: &CancellationToken) -> Router {
    router_for(config, &ToolSelection::default(), shutdown)
}

/// The HTTP application, offering the tools `tools` selects
/// ([`crate::toolsets`]).
pub fn router_for(
    config: &HttpConfig,
    tools: &ToolSelection,
    shutdown: &CancellationToken,
) -> Router {
    let mcp_config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_allowed_hosts(config.allowed_hosts.iter().cloned())
        .with_allowed_origins(config.allowed_origins.iter().cloned())
        .enforce_origin_validation()
        .with_max_request_body_bytes(MAX_BODY_BYTES)
        .with_cancellation_token(shutdown.clone());
    // Every request of one client is served by clones of one server, which
    // share its connection to the app; every client shares the tools.
    let toolbox = Toolbox::shared(tools.clone());
    let service = || {
        let mcp = ObservationMcp::sessionless().with_toolbox(Arc::clone(&toolbox));
        StreamableHttpService::new(
            move || Ok(mcp.clone()),
            Arc::new(NeverSessionManager::default()),
            mcp_config.clone(),
        )
    };
    let clients = match &config.auth {
        Auth::Tokens(tokens) => Clients::Tokens(Tokens::new(
            tokens.iter().map(|t| (t.clone(), service())).collect(),
        )),
        Auth::Disabled => Clients::Anyone(service()),
    };
    let mut app = Router::new()
        .route(MCP_PATH, any(mcp))
        .with_state(Arc::new(clients))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::SERVICE_UNAVAILABLE,
            REQUEST_TIMEOUT,
        ));
    if !config.allowed_origins.is_empty() {
        app = app.layer(cors(config.allowed_origins.clone()));
    }
    app.route("/healthz", get(|| async { "ok" }))
}

/// CORS for the allowed origins, matched as rmcp matches them, so a page
/// that may send requests can also read the answers. Preflights are
/// answered here, before authentication (browsers send them without
/// credentials). The request headers a preflight names are allowed as
/// asked, since MCP's include an open-ended `Mcp-Param-` family; the origin
/// check and the bearer token still apply to the request itself. No
/// cookies are involved, so credentials are not allowed.
fn cors(allowed: Vec<String>) -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(move |origin: &HeaderValue, _| {
            origin.to_str().is_ok_and(|o| origin_allowed(&allowed, o))
        }))
        .allow_methods([Method::GET, Method::POST, Method::DELETE])
        .allow_headers(AllowHeaders::mirror_request())
        .expose_headers([
            header::WWW_AUTHENTICATE,
            HeaderName::from_static("mcp-session-id"),
        ])
        .max_age(CORS_MAX_AGE)
}

/// Authenticates a request and hands it to its client's server.
async fn mcp(State(clients): State<Arc<Clients>>, req: Request) -> Response {
    let (client, service) = match &*clients {
        Clients::Tokens(tokens) => match tokens.authenticate(req.headers()) {
            Ok((name, service)) => (name, service.clone()),
            Err(refused) => return refused.into_response(),
        },
        Clients::Anyone(service) => ("anyone", service.clone()),
    };
    let span = tracing::info_span!("mcp", client);
    async move {
        match service.oneshot(req).await {
            Ok(res) => res.into_response(),
            Err(never) => match never {},
        }
    }
    .instrument(span)
    .await
}

/// Serves `config` until SIGINT or SIGTERM, then lets requests in flight
/// finish.
pub async fn serve(config: HttpConfig, tools: ToolSelection) -> std::io::Result<()> {
    match &config.auth {
        Auth::Tokens(tokens) => {
            let names: Vec<_> = tokens.iter().map(|t| t.name.as_str()).collect();
            tracing::info!(clients = ?names, "bearer tokens loaded");
        }
        Auth::Disabled => tracing::warn!(
            "OBS_MCP_INSECURE_NO_AUTH is set: anyone who can reach {} can use this server",
            config.listen
        ),
    }
    if !config.listen.ip().is_loopback() {
        tracing::info!(
            "listening beyond loopback: put a TLS-terminating reverse proxy in front so tokens are not sent in clear"
        );
    }
    let shutdown = CancellationToken::new();
    let app = router_for(&config, &tools, &shutdown);
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        address = %listener.local_addr()?,
        allowed_hosts = ?config.allowed_hosts,
        allowed_origins = ?config.allowed_origins,
        "observation-mcp serving on http://{}{MCP_PATH}",
        listener.local_addr()?
    );
    let stopping = CancellationToken::new();
    let server = axum::serve(listener, app).with_graceful_shutdown({
        let stopping = stopping.clone();
        async move {
            shutdown_signal().await;
            tracing::info!("shutting down");
            shutdown.cancel();
            stopping.cancel();
        }
    });
    let server = server.into_future();
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => return result,
        () = stopping.cancelled() => {}
    }
    match tokio::time::timeout(SHUTDOWN_GRACE, server).await {
        Ok(result) => result,
        Err(_) => {
            tracing::warn!("streams still open after {SHUTDOWN_GRACE:?}; stopping anyway");
            Ok(())
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!("cannot listen for Ctrl-C: {e}");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                tracing::error!("cannot listen for SIGTERM: {e}");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}
