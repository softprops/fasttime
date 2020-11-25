mod backend;
mod fastly_dictionary;
mod fastly_http_body;
mod fastly_http_req;
mod fastly_http_resp;
mod fastly_log;
mod fastly_uap;
mod geo;
mod handler;
mod memory;

use anyhow::anyhow;
use backend::Backends;
use colored::Colorize;
use handler::Handler;
use http::{
    header::HOST,
    uri::{Authority, Scheme, Uri},
    Request, Response,
};
use hyper::{
    server::conn::AddrStream,
    service::{make_service_fn, service_fn},
    Server,
};
use notify::{watcher, DebouncedEvent, RecursiveMode, Watcher};
use std::{
    collections::HashMap,
    error::{Error, Error as StdError},
    fs,
    path::{Path, PathBuf},
    process::exit,
    str::FromStr,
    sync::{mpsc::channel, Arc, RwLock},
    time::{Duration, SystemTime},
};
use structopt::StructOpt;
use tokio::task::spawn_blocking;
use wasmtime::{Engine, Module, Store};

pub type BoxError = Box<dyn Error + Send + Sync + 'static>;
type Backend = Vec<(String, String)>;
type Dictionary = Vec<(String, HashMap<String, String>)>;

fn parse_key_value<T, U>(s: &str) -> Result<(T, U), Box<dyn StdError>>
where
    T: FromStr,
    T::Err: StdError + 'static,
    U: FromStr,
    U::Err: StdError + 'static,
{
    let pos = s
        .find(':')
        .ok_or_else(|| format!("invalid KEY:value: no `:` found in `{}`", s))?;
    Ok((s[..pos].parse()?, s[pos + 1..].parse()?))
}

fn parse_dictionary(s: &str) -> Result<(String, HashMap<String, String>), Box<dyn StdError>> {
    let (name, v) = parse_key_value::<String, String>(s)?;
    let dict: Result<HashMap<String, String>, Box<dyn StdError>> =
        v.split(',').try_fold(HashMap::default(), |mut res, el| {
            let pos = el
                .find('=')
                .ok_or_else(|| format!("invalid KEY=value: no `=` found in `{}`", el))?;
            res.insert(el[..pos].parse()?, el[pos + 1..].parse()?);
            Ok(res)
        });
    Ok((name, dict?))
}

/// ⏱️  A local Fastly Compute@Edge runtime emulator
#[derive(Debug, StructOpt)]
struct Opts {
    /// Path to a Fastly Compute@Edge .wasm file
    #[structopt(long, short, default_value = "bin/main.wasm")]
    wasm: PathBuf,
    /// Port to listen on
    #[structopt(long, short, default_value = "3000")]
    port: u16,
    /// Backend to proxy in backend-name:host format (foo:foo.org)
    #[structopt(long, short, parse(try_from_str = parse_key_value))]
    backend: Vec<(String, String)>,
    /// Edge dictionary in dictionary-name:key=value,key=value format
    #[structopt(long, short, parse(try_from_str = parse_dictionary))]
    dictionary: Vec<(String, HashMap<String, String>)>,
    /// Watch for changes to .wasm file, reloading application when relevant
    #[structopt(long)]
    watch: bool,
}

// re-writing uri to add host and authority. fastly requests validate these are present before sending them upstream
fn rewrite_uri(req: Request<hyper::Body>) -> Result<Request<hyper::Body>, BoxError> {
    let mut req = req;
    let mut uri = req.uri().clone().into_parts();
    uri.scheme = Some(Scheme::HTTP);
    uri.authority = req
        .headers()
        .get(HOST)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| match s.parse::<Authority>() {
            Ok(a) => Some(a),
            Err(e) => {
                log::debug!("Failed to parse host header as authority: {}", e);
                None
            }
        });
    *req.uri_mut() = Uri::from_parts(uri)?;
    Ok(req)
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

#[derive(Clone)]
struct State {
    module: Module,
    engine: Engine,
    backend: Backend,
    dictionary: Dictionary,
}

async fn run(opts: Opts) -> Result<(), BoxError> {
    let Opts {
        wasm,
        port,
        backend,
        dictionary,
        watch,
    } = opts;
    let engine = Engine::default();

    let module = load_module(&engine, &wasm, true)?;

    let addr = ([127, 0, 0, 1], port).into();
    let state = State {
        module,
        engine: engine.clone(),
        backend: backend.clone(),
        dictionary: dictionary.clone(),
    };
    // Arc because we'll be using it in multiple threads, RwLock because we only
    // write to it when we're updating the module.
    let state = Arc::new(RwLock::new(state));
    let moved_state = state.clone();
    let server = Server::try_bind(&addr)?.serve(make_service_fn(move |conn: &AddrStream| {
        let state = moved_state.clone();
        let client_ip = conn.remote_addr().ip();
        async move {
            Ok::<_, anyhow::Error>(service_fn(move |req| {
                let State {
                    module,
                    engine,
                    backend,
                    dictionary,
                } = state.read().unwrap().clone();
                async move {
                    Ok::<Response<hyper::Body>, anyhow::Error>(
                        spawn_blocking(move || {
                            Handler::new(rewrite_uri(req).expect("invalid uri"))
                                .run(
                                    &module,
                                    Store::new(&engine),
                                    if backend.is_empty() {
                                        backend::default()
                                    } else {
                                        Box::new(backend::Proxy::new(backend.into_iter().collect()))
                                    },
                                    dictionary.into_iter().collect(),
                                    client_ip,
                                )
                                .map_err(|e| {
                                    log::debug!("Handler::run error: {}", e);
                                    anyhow!(e.to_string())
                                })
                        })
                        .await??,
                    )
                }
            }))
        }
    }));

    println!(" {} Listening on http://{}", "●".bold().green(), addr);
    if !backend.is_empty() {
        println!("   {} Backends", "❯".dimmed());
        for (name, host) in backend {
            println!("     {} > {}", name, host);
        }
    }

    // assign to something to prevent watch resources from being dropped
    let _watcher = if watch {
        Some(monitor(&wasm, engine, state)?)
    } else {
        None
    };

    server.await?;

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
    if let Err(e) = run(Opts::from_args()).await {
        eprintln!(" {} error: {}", "✖".bold().red(), e);
        exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::body::{to_bytes, Body};

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
        Ok(std::str::from_utf8(&to_bytes(resp.into_body()).await?)?.to_owned())
    }

    #[test]
    fn test_rewrite_uri() -> Result<(), BoxError> {
        let req = Request::builder()
            .uri("/foo")
            .header(HOST, "fasttime.co")
            .body(hyper::Body::empty())?;
        let rewritten = rewrite_uri(req)?;
        assert_eq!(
            rewritten.uri().authority(),
            Some(&"fasttime.co".parse::<Authority>()?)
        );
        assert_eq!(rewritten.uri().scheme().map(Scheme::as_str), Some("http"));
        Ok(())
    }
}
