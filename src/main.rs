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
    /// Path to .wasm file
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

fn load_module(engine: &Engine, file: impl AsRef<Path>) -> anyhow::Result<Module> {
    // Loading a module significant amount of time depending on the size
    // of the module but only needs to happen once per application
    println!("{}  Loading module...", " ◌".dimmed());
    let s = SystemTime::now();
    let module = Module::from_file(&engine, file)?;
    println!(
        " {} Loaded module in {:?} ✨",
        "✔".bold().green(),
        s.elapsed().unwrap_or_default()
    );
    Ok(module)
}

type Backend = Vec<(String, String)>;
type Dictionary = Vec<(String, HashMap<String, String>)>;

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
    } = opts;
    let engine = Engine::default();

    let module = load_module(&engine, &wasm)?;

    let addr = ([127, 0, 0, 1], port).into();
    let state = State {
        module,
        engine: engine.clone(),
        backend: backend.clone() as Backend,
        dictionary: dictionary.clone() as Dictionary,
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
                let State { module, engine, backend, dictionary } = state.read().unwrap().clone();
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

    // For receiving events from notify's watcher
    let (tx, rx) = channel();
    // Create a watcher object, delivering debounced events. The Duration is how
    // long the watcher waits after each raw event to combine things into one
    // debouced event. XXX: Make CLI option?
    let mut watcher = watcher(tx, Duration::from_secs(1)).unwrap();

    // Monitor the parent, because deleting the file removes the watch on some
    // platforms, but not all. So monitor the directory it's in, and then filter
    // for the specific file. Canonicalize because the watcher deals in absolute
    // paths. (Or at least it does on Linux.)
    let wasm = fs::canonicalize(wasm).unwrap();
    let wasmdir = &wasm.parent().unwrap();
    watcher.watch(wasmdir, RecursiveMode::Recursive).unwrap();

    // Unfortunately notify's watcher doesn't work with async channels, so let's
    // have a thread for the blocking read from that.
    spawn_blocking(move || {
        loop {
            let event = rx.recv();
            match &event {
                Ok(DebouncedEvent::Chmod(path))
                | Ok(DebouncedEvent::Create(path))
                | Ok(DebouncedEvent::Rename(_, path))
                | Ok(DebouncedEvent::Remove(path))
                | Ok(DebouncedEvent::Write(path)) => {
                    if *path == wasm {
                        log::debug!("notify: {:?}", event.unwrap());
                        if let Ok(module) = load_module(&engine, &wasm) {
                            log::debug!("replacing module");
                            state.write().unwrap().module = module;
                        }
                    }
                }
                Err(e) => log::debug!("watch error: {:?}", e),
                _ => (),
            }
        }
    });

    server.await?;

    Ok(())
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
