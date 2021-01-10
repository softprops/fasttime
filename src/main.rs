//! Fastly allows you to run WASM request handlers within a WASI-based runtime hosted on its managed edge servers. fasttime implements those runtime interfaces using wasmtime serving up your application on a local HTTP server allowing you to run you Compute@Edge applications ✨ locally on your laptop ✨.

mod backend;
#[doc(hidden)]
mod fastly_dictionary;
#[doc(hidden)]
mod fastly_http_body;
#[doc(hidden)]
mod fastly_http_req;
#[doc(hidden)]
mod fastly_http_resp;
#[doc(hidden)]
mod fastly_log;
#[doc(hidden)]
mod fastly_uap;
mod geo;
mod handler;
mod memory;
mod opts;

use anyhow::anyhow;

use backend::{Backend, Backends};
use chrono::offset::Local;
use colored::Colorize;
use core::task::{Context, Poll};
use futures_util::{
    future::{ready, TryFutureExt},
    stream::{Stream, StreamExt},
};
use handler::Handler;
use http::{
    header::HOST,
    uri::{Authority, Scheme, Uri},
    Request, Response,
};
use hyper::{
    server::conn::AddrStream,
    service::{make_service_fn, service_fn},
    Body, Server,
};
use notify::{watcher, DebouncedEvent, RecursiveMode, Watcher};
use opts::Opts;
use rustls::internal::pemfile;
use serde_derive::Deserialize;
use std::{
    collections::HashMap,
    error::Error,
    fs::{self, File},
    io::BufReader,
    net::IpAddr,
    path::{Path, PathBuf},
    pin::Pin,
    process::exit,
    sync::{mpsc::channel, Arc, RwLock},
    time::{Duration, Instant, SystemTime},
};
use tokio::{
    net::{TcpListener, TcpStream},
    task::spawn_blocking,
};
use tokio_rustls::{server::TlsStream, TlsAcceptor};
use wasmtime::{Engine, Module, Store};

pub type BoxError = Box<dyn Error + Send + Sync + 'static>;

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct Dictionary {
    name: String,
    entries: HashMap<String, String>,
}

// re-writing uri to add host and authority. fastly requests validate these are present before sending them upstream
fn rewrite_uri(
    req: Request<Body>,
    scheme: Scheme,
) -> Result<Request<Body>, BoxError> {
    let mut req = req;
    let mut uri = req.uri().clone().into_parts();
    uri.scheme = Some(scheme);

    uri.authority = req.uri().authority().cloned().or_else(|| {
        req.headers()
            .get(HOST)
            .and_then(|h| h.to_str().ok())
            .and_then(|s| match s.parse::<Authority>() {
                Ok(a) => Some(a),
                Err(e) => {
                    log::debug!("Failed to parse host header as authority: {}", e);
                    None
                }
            })
    });
    *req.uri_mut() = Uri::from_parts(uri)?;
    Ok(req)
}

fn log_prefix(
    req: &Request<Body>,
    client_ip: &Option<IpAddr>,
) -> String {
    format!(
        "{} \"{} {} {}\"",
        format!(
            "{} - - [{}]",
            client_ip
                .map(|ip| ip.to_string())
                .unwrap_or_else(|| "-".into()),
            Local::now().to_rfc3339()
        )
        .dimmed(),
        req.method(),
        req.uri().path(),
        format!("{:?}", req.version())
    )
}

fn log_suffix(
    resp: &Response<Body>,
    start: Instant,
) -> String {
    format!(
        "{} {}",
        match resp.status().as_u16() {
            redir @ 300..=399 => redir.to_string().yellow(),
            client @ 400..=499 => client.to_string().red(),
            server @ 500..=599 => server.to_string().red(),
            ok => ok.to_string().green(),
        },
        format!("{:.2?}", start.elapsed()).dimmed()
    )
}

struct HyperAcceptor<'a> {
    acceptor: Pin<Box<dyn Stream<Item = Result<TlsStream<TcpStream>, anyhow::Error>> + 'a>>,
}

impl hyper::server::accept::Accept for HyperAcceptor<'_> {
    type Conn = TlsStream<TcpStream>;
    type Error = anyhow::Error;

    fn poll_accept(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
    ) -> Poll<Option<Result<Self::Conn, Self::Error>>> {
        Pin::new(&mut self.acceptor).poll_next(cx)
    }
}

fn load_module(
    engine: &Engine,
    file: impl AsRef<Path>,
    first_load: bool,
) -> anyhow::Result<Module> {
    // Loading a module significant amount of time depending on the size
    // of the module but only needs to happen once per application
    println!(
        "{}  {}oading module...",
        " ◌".dimmed(),
        if first_load { "L" } else { "Rel" }
    );
    let s = SystemTime::now();
    let module = Module::from_file(&engine, file)?;
    println!(
        " {} {}oaded module in {:?} ✨",
        "✔".bold().green(),
        if first_load { "L" } else { "Rel" },
        s.elapsed().unwrap_or_default()
    );
    Ok(module)
}

#[doc(hidden)]
#[derive(Clone)]
struct State {
    module: Module,
    engine: Engine,
    backends: Option<Vec<Backend>>,
    dictionaries: HashMap<String, HashMap<String, String>>,
}

fn tls_config(
    cert: impl AsRef<Path>,
    key: impl AsRef<Path>,
) -> Result<rustls::ServerConfig, BoxError> {
    let certs = pemfile::certs(&mut BufReader::new(File::open(cert)?));
    let key = pemfile::pkcs8_private_keys(&mut BufReader::new(File::open(key)?));
    let mut cfg = rustls::ServerConfig::new(rustls::NoClientAuth::new());
    cfg.set_single_cert(
        certs.map_err(|_| anyhow!("unable to load tls certificate"))?,
        key.map_err(|_| anyhow!("unable to load tls private key"))?[0].clone(),
    )
    .map_err(|e| anyhow!(e.to_string()))?;
    // Configure ALPN to accept HTTP/2, HTTP/1.1 in that order.
    cfg.set_protocols(&[b"h2".to_vec(), b"http/1.1".to_vec()]);
    Ok(cfg)
}

async fn run(opts: Opts) -> Result<(), BoxError> {
    let Opts {
        wasm,
        port,
        backends,
        dictionaries,
        tls_cert,
        tls_key,
        watch,
        config_file: _,
    } = opts;

    let engine = Engine::default();

    let module = load_module(&engine, &wasm, true)?;

    let addr = ([127, 0, 0, 1], port).into();

    // dictionaries of the same name can come from both the CLI params and config file,
    // so merge them here. The correct order is provided in opts.rs.
    let dictionaries: HashMap<String, HashMap<String, String>> = dictionaries
        .unwrap_or_default()
        .into_iter()
        .fold(HashMap::new(), |mut map, d| {
            map.entry(d.name).or_default().extend(d.entries.into_iter());
            map
        });

    let state = Arc::new(RwLock::new(State {
        module,
        engine: engine.clone(),
        backends: backends.clone(),
        dictionaries,
    }));
    println!("DEBUG: {:?}", state.read().unwrap().dictionaries);
    let moved_state = state.clone();

    match (tls_cert, tls_key) {
        (Some(cert), Some(key)) => {
            let tls_acceptor = TlsAcceptor::from(Arc::new(tls_config(cert, key)?));
            let tcp = TcpListener::bind(&addr).await?;
            let acceptor = async_stream::stream! {
                loop {
                    let (socket, _) = tcp.accept().await.map_err(|e|  anyhow!(format!("Incoming tpc request failed: {}", e)))?;
                    let stream = tls_acceptor.accept(socket).map_err(|e| anyhow!(format!("TLS Error: {:?}", e)));
                    yield stream.await;
                }
            }.filter(|res|  ready(res.is_ok()));
            let server = Box::new(
                Server::builder(HyperAcceptor {
                    acceptor: Box::pin(acceptor),
                })
                .serve(make_service_fn(move |conn: &TlsStream<TcpStream>| {
                    let state = moved_state.clone();
                    let client_ip = conn.get_ref().0.peer_addr().ok().map(|addr| addr.ip());
                    async move {
                        Ok::<_, anyhow::Error>(service_fn(move |req| {
                            let State {
                                module,
                                engine,
                                backends,
                                dictionaries,
                            } = state.read().unwrap().clone();
                            async move {
                                let start = Instant::now();
                                let log = log_prefix(&req, &client_ip);
                                Ok::<Response<Body>, anyhow::Error>(
                                    spawn_blocking(move || {
                                        Handler::new(
                                            rewrite_uri(req, Scheme::HTTPS).expect("invalid uri"),
                                        )
                                        .run(
                                            &module,
                                            Store::new(&engine),
                                            if let Some(backends) = backends {
                                                Box::new(backend::Proxy::new(backends))
                                            } else {
                                                backend::default()
                                            },
                                            dictionaries,
                                            client_ip,
                                        )
                                        .map_err(|e| {
                                            log::debug!("Handler::run error: {}", e);
                                            anyhow!(e.to_string())
                                        })
                                        .map(|res| {
                                            println!("{} {}", log, log_suffix(&res, start));
                                            res
                                        })
                                    })
                                    .await??,
                                )
                            }
                        }))
                    }
                })),
            );

            println!(" {} Listening on https://{}", "●".bold().green(), addr);
            if let Some(backends) = backends {
                println!("   {} Backends", "❯".dimmed());
                for b in backends {
                    println!("     {} > {}", b.name, b.address);
                }
            }

            // assign to something to prevent watch resources from being dropped
            let _watcher = if watch {
                Some(monitor(&wasm, engine, state)?)
            } else {
                None
            };
            server.await?
        }
        _ => {
            let server = Box::new(Server::try_bind(&addr)?.serve(make_service_fn(
                move |conn: &AddrStream| {
                    let state = moved_state.clone();
                    let client_ip = Some(conn.remote_addr().ip());
                    async move {
                        Ok::<_, anyhow::Error>(service_fn(move |req| {
                            let start = Instant::now();
                            let log = log_prefix(&req, &client_ip);
                            let State {
                                module,
                                engine,
                                backends,
                                dictionaries,
                            } = state.read().expect("unable to lock server state").clone();
                            async move {
                                Ok::<Response<Body>, anyhow::Error>(
                                    spawn_blocking(move || {
                                        Handler::new(
                                            rewrite_uri(req, Scheme::HTTP).expect("invalid uri"),
                                        )
                                        .run(
                                            &module,
                                            Store::new(&engine),
                                            if let Some(backends) = backends {
                                                Box::new(backend::Proxy::new(backends))
                                            } else {
                                                backend::default()
                                            },
                                            dictionaries,
                                            client_ip,
                                        )
                                        .map_err(|e| {
                                            log::debug!("Handler::run error: {}", e);
                                            anyhow!(e.to_string())
                                        })
                                        .map(|res| {
                                            println!("{} {}", log, log_suffix(&res, start));
                                            res
                                        })
                                    })
                                    .await??,
                                )
                            }
                        }))
                    }
                },
            )));

            println!(" {} Listening on http://{}", "●".bold().green(), addr);
            if let Some(backends) = backends {
                println!("   {} Backends", "❯".dimmed());
                for b in backends {
                    println!("     {} > {}", b.name, b.address);
                }
            }

            // assign to something to prevent watch resources from being dropped
            let _watcher = if watch {
                Some(monitor(&wasm, engine, state)?)
            } else {
                None
            };

            server.await?;
        }
    };

    // server.await?;

    Ok(())
}

fn monitor(
    wasm: &PathBuf,
    engine: Engine,
    state: Arc<RwLock<State>>,
) -> Result<(notify::RecommendedWatcher, tokio::task::JoinHandle<()>), BoxError> {
    // For receiving events from notify's watcher
    let (tx, rx) = channel();
    // Create a watcher object, delivering debounced events. The Duration is how
    // long the watcher waits after each raw event to combine things into one
    // debounced event.
    let mut watcher = watcher(tx, Duration::from_secs(1))?;

    // Monitor the parent, because deleting the file removes the watch on some
    // platforms, but not all. So monitor the directory it's in, and then filter
    // for the specific file. Canonicalize because the watcher deals in absolute
    // paths. (Or at least it does on Linux.)
    let wasm = fs::canonicalize(wasm)?;
    let wasmdir = &wasm.parent().expect("expected parent directory to exist");
    println!(" Watching for changes...");
    watcher.watch(wasmdir, RecursiveMode::Recursive)?;

    // Unfortunately notify's watcher doesn't work with async channels, so let's
    // have a thread for the blocking read from that.
    let handle = spawn_blocking(move || loop {
        let event = rx.recv();
        match &event {
            Ok(DebouncedEvent::Chmod(path))
            | Ok(DebouncedEvent::Create(path))
            | Ok(DebouncedEvent::Rename(_, path))
            | Ok(DebouncedEvent::Remove(path))
            | Ok(DebouncedEvent::Write(path)) => {
                if *path == wasm {
                    log::trace!("notify: {:?}", event);
                    if let Ok(module) = load_module(&engine, &wasm, false) {
                        match state.write() {
                            Ok(mut guard) => guard.module = module,
                            _ => break,
                        }
                    }
                }
            }
            Err(e) => {
                log::trace!("watch error: {:?}", e);
                break;
            }
            _ => (),
        }
    });
    Ok((watcher, handle))
}

#[tokio::main]
async fn main() {
    pretty_env_logger::init();
    if let Err(e) = run(Opts::merge_from_args_and_toml()).await {
        eprintln!(" {} error: {}", "✖".bold().red(), e);
        exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::body::to_bytes;
    use std::str;

    lazy_static::lazy_static! {
        pub (crate) static ref WASM: Option<(Engine, Module)> =
            match Path::new("./tests/app/target/wasm32-wasi/release/app.wasm") {
                path if !path.exists() => {
                    pretty_env_logger::init();
                    log::debug!("test wasm app is absent. will skip wasm tests");
                    None
                }
                path => {
                    pretty_env_logger::init();
                    log::debug!("loading wasm for test");
                    let engine = Engine::default();
                    Module::from_file(&engine, path)
                        .ok()
                        .map(|module| (engine, module))
                }
            };
    }

    pub(crate) async fn body(resp: Response<Body>) -> Result<String, BoxError> {
        Ok(str::from_utf8(&to_bytes(resp.into_body()).await?)?.to_owned())
    }

    #[test]
    fn test_rewrite_uri_http() -> Result<(), BoxError> {
        let req = Request::builder()
            .uri("/foo")
            .header(HOST, "fasttime.co")
            .body(Body::empty())?;
        let rewritten = rewrite_uri(req, Scheme::HTTP)?;
        assert_eq!(
            rewritten.uri().authority(),
            Some(&"fasttime.co".parse::<Authority>()?)
        );
        assert_eq!(rewritten.uri().scheme().map(Scheme::as_str), Some("http"));
        Ok(())
    }

    #[test]
    fn test_rewrite_uri_https() -> Result<(), BoxError> {
        let req = Request::builder()
            .uri("/foo")
            .header(HOST, "fasttime.co")
            .body(hyper::Body::empty())?;
        let rewritten = rewrite_uri(req, Scheme::HTTPS)?;
        assert_eq!(
            rewritten.uri().authority(),
            Some(&"fasttime.co".parse::<Authority>()?)
        );
        assert_eq!(rewritten.uri().scheme().map(Scheme::as_str), Some("https"));
        Ok(())
    }
}
