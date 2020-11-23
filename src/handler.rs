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

        linker.func("fastly_abi", "init", self.one_i64("fastly_abi:init"))?;

        linker.define("fastly_uap", "parse", crate::fastly_uap::parse(&store))?;

        crate::fastly_dictionary::add_to_linker(&mut linker, self.clone(), &store, dictionaries)?;

        // fastly log funcs

        linker
            .define(
                "fastly_log",
                "endpoint_get",
                crate::fastly_log::endpoint_get(self.clone(), &store),
            )?
            .define(
                "fastly_log",
                "write",
                crate::fastly_log::write(self.clone(), &store),
            )?;

        // fastly request funcs

        linker
            .func(
                "fastly_http_req",
                "pending_req_poll",
                self.none("fastly_http_req::pending_req_poll"),
            )?
            .func(
                "fastly_http_req",
                "pending_req_select",
                self.none("fastly_http_req::pending_req_select"),
            )?
            .func(
                "fastly_http_req",
                "req_downstream_tls_cipher_openssl_name",
                self.none("fastly_http_req::req_downstream_tls_cipher_openssl_name"),
            )?
            .func(
                "fastly_http_req",
                "req_downstream_tls_protocol",
                self.none("fastly_http_req::req_downstream_tls_protocol"),
            )?
            .func(
                "fastly_http_req",
                "downstream_tls_client_hello",
                self.none("fastly_http_req::downstream_tls_client_hello"),
            )?
            .func(
                "fastly_http_req",
                "header_insert",
                self.none("fastly_http_req::header_insert"),
            )?
            .func(
                "fastly_http_req",
                "send_async",
                self.none("fastly_http_req::send_async"),
            )?
            .define(
                "fastly_http_req",
                "original_header_count",
                crate::fastly_http_req::original_header_count(self.clone(), &store),
            )?
            .func(
                "fastly_http_req",
                "header_remove",
                self.none("fastly_http_req::header_remove"),
            )?
            .define(
                "fastly_http_req",
                "body_downstream_get",
                crate::fastly_http_req::body_downstream_get(self.clone(), &store),
            )?
            .define(
                "fastly_http_req",
                "downstream_client_ip_addr",
                crate::fastly_http_req::downstream_client_ip_addr(self.clone(), &store, ip),
            )?
            .define(
                "fastly_http_req",
                "new",
                crate::fastly_http_req::new(self.clone(), &store),
            )?
            .define(
                "fastly_http_req",
                "version_get",
                crate::fastly_http_req::version_get(self.clone(), &store),
            )?
            .define(
                "fastly_http_req",
                "version_set",
                crate::fastly_http_req::version_set(self.clone(), &store),
            )?
            .define(
                "fastly_http_req",
                "method_get",
                crate::fastly_http_req::method_get(self.clone(), &store),
            )?
            .define(
                "fastly_http_req",
                "method_set",
                crate::fastly_http_req::method_set(self.clone(), &store),
            )?
            .define(
                "fastly_http_req",
                "uri_get",
                crate::fastly_http_req::uri_get(self.clone(), &store),
            )?
            .define(
                "fastly_http_req",
                "uri_set",
                crate::fastly_http_req::uri_set(self.clone(), &store),
            )?
            .define(
                "fastly_http_req",
                "header_names_get",
                crate::fastly_http_req::header_names_get(self.clone(), &store),
            )?
            .define(
                "fastly_http_req",
                "header_values_get",
                crate::fastly_http_req::header_values_get(self.clone(), &store),
            )?
            .define(
                "fastly_http_req",
                "header_values_set",
                crate::fastly_http_req::header_values_set(self.clone(), &store),
            )?
            .define(
                "fastly_http_req",
                "send",
                crate::fastly_http_req::send(self.clone(), &store, backends),
            )?
            .define(
                "fastly_http_req",
                "cache_override_set",
                crate::fastly_http_req::cache_override_set(self.clone(), &store),
            )?
            .define(
                "fastly_http_req",
                "cache_override_v2_set",
                crate::fastly_http_req::cache_override_v2_set(self.clone(), &store),
            )?
            .define(
                "fastly_http_req",
                "original_header_names_get",
                crate::fastly_http_req::original_header_names_get(self.clone(), &store),
            )?;

        // fastly response funcs

        linker
            .func(
                "fastly_http_resp",
                "header_append",
                self.none("fastly_http_resp::header_append"),
            )?
            .func(
                "fastly_http_resp",
                "header_insert",
                self.none("fastly_http_resp::header_insert"),
            )?
            .func(
                "fastly_http_resp",
                "header_value_get",
                self.none("fastly_http_resp::header_value_get"),
            )?
            .func(
                "fastly_http_resp",
                "header_remove",
                self.none("fastly_http_resp::header_remove"),
            )?
            .define(
                "fastly_http_resp",
                "new",
                crate::fastly_http_resp::new(self.clone(), &store),
            )?
            .define(
                "fastly_http_resp",
                "send_downstream",
                crate::fastly_http_resp::send_downstream(self.clone(), &store),
            )?
            .define(
                "fastly_http_resp",
                "status_get",
                crate::fastly_http_resp::status_get(self.clone(), &store),
            )?
            .define(
                "fastly_http_resp",
                "status_set",
                crate::fastly_http_resp::status_set(self.clone(), &store),
            )?
            .define(
                "fastly_http_resp",
                "version_get",
                crate::fastly_http_resp::version_get(self.clone(), &store),
            )?
            .define(
                "fastly_http_resp",
                "version_set",
                crate::fastly_http_resp::version_set(self.clone(), &store),
            )?
            .define(
                "fastly_http_resp",
                "header_names_get",
                crate::fastly_http_resp::header_names_get(self.clone(), &store),
            )?
            .define(
                "fastly_http_resp",
                "header_values_get",
                crate::fastly_http_resp::header_values_get(self.clone(), &store),
            )?
            .define(
                "fastly_http_resp",
                "header_values_set",
                crate::fastly_http_resp::header_values_set(self.clone(), &store),
            )?;

        // body funcs

        linker
            .func(
                "fastly_http_body",
                "close",
                self.one("fastly_http_body::close"),
            )?
            .define(
                "fastly_http_body",
                "new",
                crate::fastly_http_body::new(self.clone(), &store),
            )?
            .define(
                "fastly_http_body",
                "write",
                crate::fastly_http_body::write(self.clone(), &store),
            )?
            .define(
                "fastly_http_body",
                "read",
                crate::fastly_http_body::read(self.clone(), &store),
            )?
            .func("fastly_http_body", "append", || {
                debug!("fastly_http_body::append");
                FastlyStatus::OK.code
            })?;

        Ok(linker)
    }

    // stubs

    fn none(
        &self,
        name: &'static str,
    ) -> impl Fn() -> i32 {
        move || {
            debug!("{} (stub)", name);
            FastlyStatus::UNSUPPORTED.code
        }
    }

    fn one_i64(
        &self,
        name: &'static str,
    ) -> impl Fn(i64) -> i32 {
        move |_: i64| {
            debug!("{} (stub)", name);
            FastlyStatus::UNSUPPORTED.code
        }
    }

    fn one(
        &self,
        name: &'static str,
    ) -> impl Fn(i32) -> i32 {
        move |_: i32| {
            debug!("{} (stub)", name);
            FastlyStatus::UNSUPPORTED.code
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::Request;
    use std::path::Path;
    use wasmtime::Engine;

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
            crate::backend::default(),
            HashMap::default(),
            "127.0.0.1".parse()?,
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
