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
use backend::Backend;
use http::{
    header::HOST,
    uri::{Authority, Scheme, Uri},
    Response,
};
mod convert;
use tokio::task::spawn_blocking;

pub type BoxError = Box<dyn Error + Send + Sync + 'static>;

/// ⏱️  A local Fastly Compute@Edge runtime emulator
#[derive(Debug, StructOpt)]
struct Opts {
    /// Path to .wasm file
    #[structopt(long, short)]
    wasm: PathBuf,
    /// Port to listen on (defaults to 3000)
    #[structopt(long, short, default_value = "3000")]
    port: u16,
    /// Backend to proxy
    #[structopt(long, short)]
    backend: Option<String>,
}

async fn run(opts: Opts) -> Result<(), BoxError> {
    let Opts {
        wasm,
        port,
        backend,
    } = opts;
    let engine = Engine::default();

    // Loading a module significant amount of time depending on the size
    // of the module but only needs to happen once per application
    println!("⏱️  Loading module...");
    let s = SystemTime::now();
    let module = Module::from_file(&engine, wasm)?;
    println!(
        " ✔ Loaded module in {:?} ✨",
        s.elapsed().unwrap_or_default()
    );

    let addr = ([127, 0, 0, 1], port).into();
    let state = (module, engine, backend);
    let server = Server::bind(&addr).serve(make_service_fn(move |_| {
        let state = state.clone();
        async move {
            Ok::<_, anyhow::Error>(service_fn(move |mut req| {
                let (module, engine, backend) = state.clone();
                async move {
                    // re-writing uri to add host and authority. fastly requests validate these are present before sending them upstream
                    let mut uri = req.uri().clone().into_parts();
                    uri.scheme = Some(Scheme::HTTP);
                    uri.authority = req
                        .headers()
                        .get(HOST)
                        .and_then(|h| h.to_str().ok())
                        .and_then(|s| s.parse::<Authority>().ok());
                    *req.uri_mut() = Uri::from_parts(uri)?;

                    Ok::<Response<hyper::Body>, anyhow::Error>(
                        spawn_blocking(move || {
                            Handler::new(req)
                                .run(
                                    &module,
                                    Store::new(&engine),
                                    backend.map_or_else::<Box<dyn backend::Backend>, _, _>(
                                        || Box::new(backend::default()),
                                        |host| Box::new(backend::Proxy::new(host)),
                                    ),
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

    println!("🟢 Listening on http://{}", addr);

    server.await?;

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    pretty_env_logger::init();
    run(Opts::from_args()).await?;
    Ok(())
}
