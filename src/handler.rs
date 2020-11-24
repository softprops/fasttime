use crate::BoxError;
use fastly_shared::FastlyStatus;
use http::{request::Parts as RequestParts, response::Parts as ResponseParts};
use hyper::{Body, Request, Response};
use log::debug;
use std::{cell::RefCell, collections::HashMap, net::IpAddr, rc::Rc};
use wasmtime::{Linker, Module, Store, Trap};
use wasmtime_wasi::{Wasi, WasiCtxBuilder};

#[derive(Debug, Default)]
pub struct Endpoint(pub String);

impl Endpoint {
    pub fn log(
        &self,
        msg: &str,
    ) {
        print!("{}", msg);
    }
}
/// Represents state within a given request/response cycle
///
/// an inbound request is provided by our driving server
///
/// a handler may send any ammount of outbound requests and build a response
#[derive(Default, Debug)]
pub struct Inner {
    /// downstream request
    pub request: Option<Request<Body>>,
    /// requests initiated within the handler
    pub requests: Vec<RequestParts>,
    /// responses from the requests initiated within the handler
    pub responses: Vec<ResponseParts>,
    /// bodies created within the handler
    pub bodies: Vec<Body>,
    /// final handler response
    pub response: Response<Body>,
    /// list of loaded dictionaries
    pub dictionaries: Vec<HashMap<String, String>>,
    /// list of loaded log endpoints
    pub endpoints: Vec<Endpoint>,
}

#[derive(Default, Clone)]
pub struct Handler {
    pub inner: Rc<RefCell<Inner>>,
}

impl Handler {
    fn into_response(self) -> Response<Body> {
        self.inner.replace(Default::default()).response
    }
}

impl Handler {
    pub fn new(request: hyper::Request<Body>) -> Self {
        Handler {
            inner: Rc::new(RefCell::new(Inner {
                request: Some(request),
                ..Inner::default()
            })),
        }
    }

    /// Runs a Request to completion for a given `Module` and `Store`
    pub fn run(
        mut self,
        module: &Module,
        store: Store,
        backends: Box<dyn crate::Backends>,
        dicionaries: HashMap<String, HashMap<String, String>>,
        ip: IpAddr,
    ) -> Result<Response<Body>, BoxError> {
        if let Some(func) = self
            .linker(store, backends, dicionaries, ip)?
            .instantiate(&module)?
            .get_func("_start")
        {
            func.call(&[])?;
        } else {
            return Err(Trap::new("wasm module does not define a `_start` func").into());
        }
        Ok(self.into_response())
    }

    /// Builds a new linker given a provided `Store`
    /// configured with WASI and Fastly sys func implementations
    fn linker(
        &mut self,
        store: Store,
        backends: Box<dyn crate::Backends>,
        dictionaries: HashMap<String, HashMap<String, String>>,
        ip: IpAddr,
    ) -> Result<Linker, BoxError> {
        let wasi = Wasi::new(
            &store,
            WasiCtxBuilder::new()
                .inherit_stdout()
                .inherit_stderr()
                .build()?,
        );
        let mut linker = Linker::new(&store);

        // add wasi funcs
        wasi.add_to_linker(&mut linker)?;

        // fill in the [`fastly-sys`](https://crates.io/crates/fastly-sys) funcs

        linker.func("fastly_abi", "init", |version: i64| {
            debug!("fastly_abi::init version={}", version);
            FastlyStatus::OK.code
        })?;

        crate::fastly_uap::add_to_linker(&mut linker, &store)?;
        crate::fastly_dictionary::add_to_linker(&mut linker, self.clone(), &store, dictionaries)?;
        crate::fastly_http_body::add_to_linker(&mut linker, self.clone(), &store)?;
        crate::fastly_log::add_to_linker(&mut linker, self.clone(), &store)?;
        crate::fastly_http_req::add_to_linker(&mut linker, self.clone(), &store, backends, ip)?;
        crate::fastly_http_resp::add_to_linker(&mut linker, self.clone(), &store)?;

        Ok(linker)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::{body::to_bytes, Request};
    use lazy_static::lazy_static;
    use std::{path::Path, str};
    use wasmtime::Engine;

    lazy_static! {
        static ref WASM: Option<(Engine, Module)> =
            match Path::new("./tests/app/target/wasm32-wasi/release/app.wasm") {
                path if !path.exists() => {
                    pretty_env_logger::init();
                    debug!("test wasm app is absent. will skip wasm tests");
                    None
                }
                path => {
                    pretty_env_logger::init();
                    debug!("loading wasm for test");
                    let engine = Engine::default();
                    Module::from_file(&engine, path)
                        .ok()
                        .map(|module| (engine, module))
                }
            };
    }

    async fn body(resp: Response<Body>) -> Result<String, BoxError> {
        Ok(str::from_utf8(&to_bytes(resp.into_body()).await?)?.to_owned())
    }

    #[tokio::test]
    async fn it_works() -> Result<(), BoxError> {
        match WASM.as_ref() {
            None => Ok(()),
            Some((engine, module)) => {
                let resp = Handler::new(Request::default()).run(
                    &module,
                    Store::new(&engine),
                    crate::backend::default(),
                    HashMap::default(),
                    "127.0.0.1".parse()?,
                )?;
                assert_eq!("Welcome to Fastly Compute@Edge!", body(resp).await?);
                Ok(())
            }
        }
    }

    #[tokio::test]
    async fn dictionary_hits_work() -> Result<(), BoxError> {
        match WASM.as_ref() {
            None => Ok(()),
            Some((engine, module)) => {
                let mut dictionaries = HashMap::new();
                let mut dictionary = HashMap::new();
                dictionary.insert("foo".to_string(), "bar".to_string());
                dictionaries.insert("dict".to_string(), dictionary);
                let resp = Handler::new(Request::get("/dictionary-hit").body(Default::default())?)
                    .run(
                        &module,
                        Store::new(&engine),
                        crate::backend::default(),
                        dictionaries,
                        "127.0.0.1".parse()?,
                    )?;
                assert_eq!("dict::foo is bar", body(resp).await?);
                Ok(())
            }
        }
    }

    #[tokio::test]
    #[ignore]
    async fn dictionary_misses_work() -> Result<(), BoxError> {
        match WASM.as_ref() {
            None => Ok(()),
            Some((engine, module)) => {
                match Handler::new(Request::get("/dictionary-miss").body(Default::default())?).run(
                    &module,
                    Store::new(&engine),
                    crate::backend::default(),
                    HashMap::default(),
                    "127.0.0.1".parse()?,
                ) {
                    Ok(_) => panic!("expected error"),
                    Err(e) => assert_eq!(e.to_string(), "test"),
                }
                Ok(())
            }
        }
    }

    #[tokio::test]
    async fn downstream_original_header_count_works() -> Result<(), BoxError> {
        match WASM.as_ref() {
            None => Ok(()),
            Some((engine, module)) => {
                let resp = Handler::new(
                    Request::get("/downstream_original_header_count")
                        .header("foo", "bar")
                        .body(Default::default())?,
                )
                .run(
                    &module,
                    Store::new(&engine),
                    crate::backend::default(),
                    HashMap::default(),
                    "127.0.0.1".parse()?,
                )?;
                assert_eq!("downstream_original_header_count 1", body(resp).await?);
                Ok(())
            }
        }
    }

    #[tokio::test]
    async fn downstream_client_ip_addr_works() -> Result<(), BoxError> {
        match WASM.as_ref() {
            None => Ok(()),
            Some((engine, module)) => {
                let resp = Handler::new(
                    Request::get("/downstream_client_ip_addr").body(Default::default())?,
                )
                .run(
                    &module,
                    Store::new(&engine),
                    crate::backend::default(),
                    HashMap::default(),
                    "127.0.0.1".parse()?,
                )?;
                assert_eq!(
                    "downstream_client_ip_addr Some(V4(127.0.0.1))",
                    body(resp).await?
                );
                Ok(())
            }
        }
    }
}
