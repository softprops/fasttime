use anyhow::anyhow;
use hyper::{
    service::{make_service_fn, service_fn},
    Server,
};
use std::{error::Error, path::PathBuf, time::SystemTime};
use structopt::StructOpt;
use wasmtime::{Engine, Module, Store};
mod handler;
mod memory;
use handler::Handler;
mod backend;
use backend::Backends;
use http::{
    header::HOST,
    uri::{Authority, Scheme, Uri},
    Request, Response,
};
mod convert;
use colored::Colorize;
use std::{collections::HashMap, error::Error as StdError, process::exit, str::FromStr};
use tokio::task::spawn_blocking;

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
    #[structopt(long, short)]
    wasm: PathBuf,
    /// Port to listen on (defaults to 3000)
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

async fn run(opts: Opts) -> Result<(), BoxError> {
    let Opts {
        wasm,
        port,
        backend,
        dictionary,
    } = opts;
    let engine = Engine::default();

    // Loading a module significant amount of time depending on the size
    // of the module but only needs to happen once per application
    println!("{}  Loading module...", " ◌".dimmed());
    let s = SystemTime::now();
    let module = Module::from_file(&engine, wasm)?;
    println!(
        " {} Loaded module in {:?} ✨",
        "✔".bold().green(),
        s.elapsed().unwrap_or_default()
    );

    let addr = ([127, 0, 0, 1], port).into();
    let state = (module, engine, backend.clone(), dictionary.clone());
    let server = Server::try_bind(&addr)?.serve(make_service_fn(move |_| {
        let state = state.clone();
        async move {
            Ok::<_, anyhow::Error>(service_fn(move |req| {
                let (module, engine, backend, dictionary) = state.clone();
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
