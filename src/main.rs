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
mod http;

pub type BoxError = Box<dyn Error + Send + Sync + 'static>;

/// ⏱️  A local Fastly Compute@Edge server emulator
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
                    // re-writing uri to a host and authority. fastly requests validate these are present before sending them upstream
                    *req.uri_mut() = format!(
                        "http://{}{}",
                        req.headers()
                            .get("host")
                            .and_then(|h| h.to_str().ok())
                            .unwrap(),
                        req.uri()
                    )
                    .parse()
                    .unwrap();
                    Ok::<hyper::Response<hyper::Body>, anyhow::Error>(
                        tokio::task::spawn_blocking(move || {
                            Handler::new(req)
                                .run(
                                    &module,
                                    Store::new(&engine),
                                    backend.map_or_else::<Box<dyn backend::Backend>, _, _>(
                                        || Box::new(backend::default()),
                                        |s| Box::new(backend::Proxy::new(s)),
                                    ),
                                )
                                .map_err(|e| {
                                    log::debug!("handler::run error: {}", e);
                                    anyhow!(e.to_string())
                                })
                        })
                        .await
                        .unwrap()
                        .unwrap(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::Request;
    use std::path::Path;

    #[tokio::test]
    async fn it_works() -> Result<(), BoxError> {
        if !Path::new("./tests/app/bin/main.wasm").exists() {
            return Ok(());
        }
        // todo create one eng/module for all tests
        let engine = Engine::default();
        let module = Module::from_file(&engine, "./tests/app/bin/main.wasm")?;

        let response = Handler::new(Request::default()).run(
            &module,
            Store::new(&engine),
            backend::default(),
        )?;
        println!("{:?}", response.status());
        let bytes = hyper::body::to_bytes(response.into_body()).await?;
        assert_eq!(
            "Welcome to Fastly Compute@Edge!",
            std::str::from_utf8(&bytes)?
        );
        Ok(())
    }
}
