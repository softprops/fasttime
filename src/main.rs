use anyhow::anyhow;
use fastly_shared::FastlyStatus;
use hyper::{
    service::{make_service_fn, service_fn},
    Body, Request, Response, Server,
};
use log::debug;
use std::{cell::RefCell, error::Error, path::PathBuf, rc::Rc, time::SystemTime};
use structopt::StructOpt;
use wasmtime::{Caller, Engine, Extern, Linker, Module, Store, Trap};
use wasmtime_wasi::{Wasi, WasiCtxBuilder};

mod memory;
use memory::{ReadMem, WriteMem};

type BoxError = Box<dyn Error + Send + Sync + 'static>;
type RequestHandle = i32;
type ResponseHandle = i32;
type BodyHandle = i32;

/// ⏱️  A local Fastly Compute@Edge server emulator
#[derive(Debug, StructOpt)]
struct Opts {
    /// Path to .wasm file
    #[structopt(long, short)]
    wasm: PathBuf,
    /// Port to listen on (defaults to 3000)
    #[structopt(long, short, default_value = "3000")]
    port: u16,
}

/// Represents state within a given request/response cycle
///
/// an inbound request is provided by our driving server
///
/// a handler may send any ammount of outbound requests and build a response
#[derive(Default, Debug)]
struct Inner {
    /// downstream request
    request: Request<Body>,
    /// requests initiated within the handler
    requests: Vec<Request<Body>>,
    /// bodies created within the handler
    bodies: Vec<Body>,
    /// final handler response
    response: Response<Body>,
}

#[derive(Default, Clone)]
struct Handler {
    inner: Rc<RefCell<Inner>>,
}

impl Handler {
    fn into_response(self) -> Response<Body> {
        self.inner.replace(Default::default()).response
    }
}

/// macro for getting exported memory from `Caller` or early return  on `Trap` error
macro_rules! memory {
    ($expr:expr) => {
        match $expr.get_export("memory") {
            Some(Extern::Memory(mem)) => mem,
            _ => return Err(Trap::new("failed to resolve exported host memory")),
        };
    };
}

impl Handler {
    fn new(request: hyper::Request<Body>) -> Self {
        Handler {
            inner: Rc::new(RefCell::new(Inner {
                request,
                ..Inner::default()
            })),
        }
    }

    /// Runs a Request to completion for a given `Module` and `Store`
    fn run(
        mut self,
        module: &Module,
        store: Store,
    ) -> Result<Response<Body>, BoxError> {
        if let Some(func) = self.linker(store)?.instantiate(&module)?.get_func("_start") {
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

        linker.func("fastly_uap", "parse", self.none("fastly_uap::parse"))?;

        // fastly log funcs

        linker
            .func(
                "fastly_log",
                "endpoint_get",
                self.none("fastly_log::endpoint_get"),
            )?
            .func("fastly_log", "write", self.none("fastly_log::write"))?;

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
            .func(
                "fastly_http_req",
                "original_header_count",
                self.none("fastly_http_req::original_header_count"),
            )?
            .func(
                "fastly_http_req",
                "header_remove",
                self.none("fastly_http_req::header_remove"),
            )?;

        let body_downstream_get = self.clone();
        linker
            .func(
                "fastly_http_req",
                "body_downstream_get",
                move |caller: Caller<'_>, request_handle_out: RequestHandle, body_handle_out: BodyHandle| {
                    debug!(
                        "fastly_http_req::body_downstream_get request_handle_out={} body_handle_out={}",
                        request_handle_out, body_handle_out
                    );
                    body_downstream_get
                        .inner
                        .borrow_mut()
                        .requests
                        .push(Request::new(Body::default()));
                        body_downstream_get
                        .inner
                        .borrow_mut()
                        .bodies
                        .push(Body::default());
                    let index = body_downstream_get.inner.borrow().requests.len() - 1;

                    let mut mem = memory!(caller);
                    mem.write_i32(request_handle_out as usize, index as i32);
                    mem.write_i32(body_handle_out as usize, index as i32);
                    Ok(FastlyStatus::OK.code)
                }
            )?
            .func(
                "fastly_http_req",
                "downstream_client_ip_addr",
                self.none("fastly_http_req::downstream_client_ip_addr"),
            )?
            .func("fastly_http_req", "new", self.one("fastly_http_req::new"))?
            .func(
                "fastly_http_req",
                "version_get",
                move |caller: Caller<'_>, handle: RequestHandle, version_out: i32| {
                    debug!(
                        "fastly_http_req::version_get handle={} version_out={}",
                        handle, version_out
                    );
                    // http 1/1
                    let version = 2;
                    memory!(caller).write_i32(version_out as usize, version);
                    Ok(FastlyStatus::OK.code)
                },
            )?
            .func(
                "fastly_http_req",
                "version_set",
                move |_: Caller<'_>, handle: RequestHandle, version_out: i32| {
                    debug!(
                        "fastly_http_req::version_set handle={} version_out={}",
                        handle, version_out
                    );
                    // noop

                    FastlyStatus::OK.code
                },
            )?;
        let method_get = self.clone();
        linker
            .func(
                "fastly_http_req",
                "method_get",
                move |caller: Caller<'_>,
                      handle: RequestHandle,
                      addr: i32,
                      maxlen: i32,
                      nwritten_out: i32| {
                    debug!(
                        "fastly_http_req::method_get handle={} addr={} maxlen={} nwritten_out={}",
                        handle, addr, maxlen, nwritten_out
                    );
                    let mut mem = memory!(caller);
                    let written = match mem.write(
                        addr as usize,
                        method_get
                            .inner
                            .borrow()
                            .request
                            .method()
                            .as_ref()
                            .as_bytes(),
                    ) {
                        Err(_) => {
                            return Err(Trap::new("Failed to write request HTTP method bytes"))
                        }
                        Ok(num) => num,
                    };
                    mem.write_u32(nwritten_out as usize, written as u32);
                    Ok(FastlyStatus::OK.code)
                },
            )?
            .func(
                "fastly_http_req",
                "method_set",
                self.three("fastly_http_req::method_set"),
            )?;

        let xqd_req_uri_get = self.clone();
        linker.func(
            "fastly_http_req",
            "uri_get",
            move |caller: Caller<'_>,
                  handle: RequestHandle,
                  addr: i32,
                  maxlen: i32,
                  nwritten_out: i32| {
                debug!(
                    "fastly_http_req::uri_get handle={} addr={} maxlen={} nwritten_out={}",
                    handle, addr, maxlen, nwritten_out
                );
                let mut mem = memory!(caller);
                let written = match mem.write(
                    addr as usize,
                    xqd_req_uri_get
                        .inner
                        .borrow_mut()
                        .request
                        .uri()
                        .to_string()
                        .as_bytes(),
                ) {
                    Err(_) => return Err(Trap::new("failed to write method bytes")),
                    Ok(num) => num,
                };

                mem.write_u32(nwritten_out as usize, written as u32);
                Ok(FastlyStatus::OK.code)
            },
        )?.func(
            "fastly_http_req",
            "uri_set",
            self.three("fastly_http_req::uri_set"),
        )?.func(
            "fastly_http_req",
            "header_names_get",
            |_: Caller<'_>,
             _handle: RequestHandle,
             _addr: i32,
             _maxlen: i32,
             _cursor: i32,
             _ending_cursor_out: i32,
             _nwritten_out: i32| {
                debug!("fastly_http_req::header_names_get");
                // noop
                FastlyStatus::OK.code
            },
        )?.func(
            "fastly_http_req",
            "header_values_get",
            |_handle: RequestHandle,
             _name_addr: i32,
             _name_size: i32,
             _addr: i32,
             _maxlen: i32,
             _cursor: i32,
             _ending_cursor_out: i32,
             _nwritten_out: i32| {
                debug!("fastly_http_req::header_values_get");
                // noop
                FastlyStatus::OK.code
            },
        )?.func(
            "fastly_http_req",
            "header_values_set",
            |handle: RequestHandle, name_addr: i32, name_size: i32, values_addr: i32, values_size: i32| {
                debug!("fastly_http_req::header_values_set handle={}, name_addr={} name_size={} values_addr={} values_size={}", handle, name_addr, name_size, values_addr, values_size);
                FastlyStatus::OK.code
            },
        )?.func(
            "fastly_http_req",
            "send",
            |_rhandle: RequestHandle,
             _bhandle: BodyHandle,
             _backend_addr: i32,
             _backend_size: i32,
             _wh_out: i32,
             _bh_out: i32| {
                debug!("fastly_http_req::send");
                // noop
                FastlyStatus::OK.code
            },
        )?.func(
            "fastly_http_req",
            "cache_override_set",
            self.four("fastly_http_req::cache_override_set"),
        )?.func(
            "fastly_http_req",
            "cache_override_v2_set",
            move |_caller: Caller<'_>,
                  _handle_out: RequestHandle,
                  _tag: u32,
                  _ttl: u32,
                  _swr: u32,
                  _sk: i32, // see fastly-sys types
                  _sk_len: i32| {
                debug!("fastly_http_req::cache_override_v2_set");
                // noop
                FastlyStatus::UNSUPPORTED.code
            },
        )?.func(
            "fastly_http_req",
            "original_header_names_get",
            self.none("fastly_http_req::original_header_names_get"),
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
            .func(
                "fastly_http_resp",
                "new",
                move |caller: Caller<'_>, handle_out: i32| {
                    debug!("fastly_http_resp.new handle_out={}", handle_out);
                    memory!(caller).write_u32(handle_out as usize, 1 as u32);
                    Ok(0)
                },
            )?
            .func(
                "fastly_http_resp",
                "send_downstream",
                move |_: Caller<'_>, whandle: ResponseHandle, bhandle: BodyHandle, stream: i32| {
                    debug!(
                        "fastly_http_resp::send_downstream whandle={} bhandle={} stream={}",
                        whandle, bhandle, stream
                    );

                    // noop

                    FastlyStatus::OK.code
                },
            )?
            .func(
                "fastly_http_resp",
                "status_get",
                self.two("fastly_http_resp::status_get"),
            )?;

        let status_set = self.clone();
        linker
            .func(
                "fastly_http_resp",
                "status_set",
                move |_: Caller<'_>, whandle: ResponseHandle, status: i32| {
                    debug!(
                        "fastly_http_resp::status_set whandle={} status={}",
                        whandle, status
                    );

                    *status_set.inner.borrow_mut().response.status_mut() =
                        hyper::http::StatusCode::from_u16(status as u16)
                            .map_err(|e| wasmtime::Trap::new(e.to_string()))?;

                    Ok(FastlyStatus::OK.code)
                },
            )?
            .func(
                "fastly_http_resp",
                "version_get",
                self.two("fastly_http_resp::version_get"),
            )?
            .func(
                "fastly_http_resp",
                "version_set",
                move |_: Caller<'_>, whandle: ResponseHandle, version: i32| {
                    debug!(
                        "fastly_http_resp::version_set whandle={} version={}",
                        whandle, version
                    );
                    // todo map version to http::Version enum

                    Ok(FastlyStatus::OK.code)
                },
            )?
            .func(
                "fastly_http_resp",
                "header_names_get",
                |_handle: i32,
                 _addr: i32,
                 _maxlen: i32,
                 _cursor: i32,
                 _ending_cursor_out: i32,
                 _nwritten_out: i32| {
                    debug!("fastly_http_resp::header_names_get");
                    FastlyStatus::OK.code
                },
            )?
            .func(
                "fastly_http_resp",
                "header_values_get",
                |_handle: i32,
                 _name_addr: i32,
                 _name_size: i32,
                 _addr: i32,
                 _maxlen: i32,
                 _cursor: i32,
                 _ending_cursor_out: i32,
                 _nwritten_out: i32| {
                    debug!("fastly_http_resp::header_values_get");
                    FastlyStatus::OK.code
                },
            )?
            .func(
                "fastly_http_resp",
                "header_values_set",
                |_handle: i32,
                 _name_addr: i32,
                 _name_size: i32,
                 _values_addr: i32,
                 _values_size: i32| {
                    debug!("fastly_http_resp::header_values_set");
                    FastlyStatus::OK.code
                },
            )?;

        // body funcs

        linker.func(
            "fastly_http_body",
            "close",
            self.one("fastly_http_body::close"),
        )?;

        let xqd_body_new = self.clone();
        linker.func(
            "fastly_http_body",
            "new",
            move |caller: Caller<'_>, handle_out: i32| {
                debug!("fastly_http_body::new");
                let mut inner = xqd_body_new.inner.borrow_mut();
                inner.bodies.push(Body::default());
                memory!(caller).write_u32(handle_out as usize, inner.bodies.len() as u32);

                Ok(FastlyStatus::OK.code)
            },
        )?;

        let xqd_body_write = self.clone();
        linker.func(
            "fastly_http_body",
            "write",
            move |caller: Caller<'_>,
                  handle: i32,
                  addr: i32,
                  size: i32,
                  body_end: i32,
                  nwritten_out: i32| {
                debug!(
                    "fastly_http_body::write handle={} addr={} size={} body_end={} nwritten_out={}",
                    handle, addr, size, body_end, nwritten_out
                );
                let mut mem = memory!(caller);
                let (read, buf) = match mem.read(addr as usize, size as usize) {
                    Err(_) => return Err(Trap::new("failed to read body memory")),
                    Ok((num, buf)) => (num, buf),
                };
                *xqd_body_write.inner.borrow_mut().response.body_mut() = Body::from(buf);

                //println!("body is {:#?}", std::str::from_utf8(&buf));
                mem.write_u32(nwritten_out as usize, read as u32);

                Ok(FastlyStatus::OK.code)
            },
        )?;

        linker.func("fastly_http_body", "read", || {
            debug!("fastly_http_body::read");
            FastlyStatus::OK.code
        })?;

        linker.func("fastly_http_body", "append", || {
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

    fn two(
        &self,
        name: &'static str,
    ) -> impl Fn(i32, i32) -> i32 {
        move |_: i32, _: i32| {
            debug!("{} (stub)", name);
            FastlyStatus::UNSUPPORTED.code
        }
    }

    fn three(
        &self,
        name: &'static str,
    ) -> impl Fn(i32, i32, i32) -> i32 {
        move |_: i32, _: i32, _: i32| {
            debug!("{} (stub)", name);
            FastlyStatus::UNSUPPORTED.code
        }
    }

    fn four(
        &self,
        name: &'static str,
    ) -> impl Fn(i32, i32, i32, i32) -> i32 {
        move |_: i32, _: i32, _: i32, _: i32| {
            debug!("{} (stub)", name);
            FastlyStatus::UNSUPPORTED.code
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    pretty_env_logger::init();
    let Opts { wasm, port } = Opts::from_args();
    let engine = Engine::default();

    // Loading a module significant amount of time depending on the size
    // of the module but only needs to happen once per application
    println!("⏱️ Loading module...");
    let s = SystemTime::now();
    let module = Module::from_file(&engine, wasm)?;
    println!(
        " ✔ Loaded module in {:?} ✨",
        s.elapsed().unwrap_or_default()
    );

    let addr = ([127, 0, 0, 1], port).into();
    let state = (module, engine);
    let server = Server::bind(&addr).serve(make_service_fn(move |_| {
        let state = state.clone();
        async move {
            Ok::<_, anyhow::Error>(service_fn(move |req| {
                let (module, engine) = state.clone();
                async move {
                    Ok::<_, anyhow::Error>(
                        Handler::new(req)
                            .run(&module, Store::new(&engine))
                            .map_err(|e| anyhow!(e.to_string()))?,
                    )
                }
            }))
        }
    }));

    println!("🟢 Listening on http://{}", addr);

    server.await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::Request;

    #[tokio::test]
    async fn it_works() -> Result<(), BoxError> {
        // todo create one eng/module for all tests
        let engine = Engine::default();
        let module = Module::from_file(&engine, "./tests/app/bin/main.wasm")?;

        let response = Handler::new(Request::default()).run(&module, Store::new(&engine))?;
        println!("{:?}", response.status());
        let bytes = hyper::body::to_bytes(response.into_body()).await?;
        assert_eq!(
            "Welcome to Fastly Compute@Edge!",
            std::str::from_utf8(&bytes)?
        );
        Ok(())
    }
}
