use fastly_shared::FastlyStatus;
use hyper::{Body, Request, Response};
use log::debug;
use std::{cell::RefCell, error::Error, rc::Rc};
use wasmtime::{Caller, Extern, Func, Linker, Module, Store, Trap};
use wasmtime_wasi::{Wasi, WasiCtxBuilder};

use crate::memory::{ReadMem, WriteMem};

type BoxError = Box<dyn Error + Send + Sync + 'static>;
type RequestHandle = i32;
type ResponseHandle = i32;
type BodyHandle = i32;

/// Represents state within a given request/response cycle
///
/// an inbound request is provided by our driving server
///
/// a handler may send any ammount of outbound requests and build a response
#[derive(Default, Debug)]
struct Inner {
    /// downstream request
    request: Option<Request<Body>>,
    /// requests initiated within the handler
    requests: Vec<Request<Body>>,
    /// responses from the requests initiated within the handler
    responses: Vec<Response<Body>>,
    /// bodies created within the handler
    bodies: Vec<Body>,
    /// final handler response
    response: Response<Body>,
}

#[derive(Default, Clone)]
pub struct Handler {
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
    ) -> Result<Response<Body>, BoxError> {
        if let Some(func) = self.linker(store)?.instantiate(&module)?.get_func("_start") {
            func.call(&[])?;
        } else {
            return Err(Trap::new("wasm module does not define a `_start` func").into());
        }
        Ok(self.into_response())
    }

    fn body_downstream_get(
        &self,
        store: &Store,
    ) -> Func {
        let clone = self.clone();
        Func::wrap(
            &store,
            move |caller: Caller<'_>,
                  request_handle_out: RequestHandle,
                  body_handle_out: BodyHandle| {
                debug!(
                    "fastly_http_req::body_downstream_get request_handle_out={} body_handle_out={}",
                    request_handle_out, body_handle_out
                );
                let index = clone.inner.borrow().requests.len();
                let (parts, body) = clone
                    .inner
                    .borrow_mut()
                    .request
                    .take()
                    .unwrap()
                    .into_parts();
                clone
                    .inner
                    .borrow_mut()
                    .requests
                    .push(Request::from_parts(parts, Body::default()));
                clone.inner.borrow_mut().bodies.push(body);

                let mut mem = memory!(caller);
                mem.write_i32(request_handle_out as usize, index as i32);
                mem.write_i32(body_handle_out as usize, index as i32);
                Ok(FastlyStatus::OK.code)
            },
        )
    }

    fn fastly_http_resp_send_downstream(
        &self,
        store: &Store,
    ) -> Func {
        let clone = self.clone();
        Func::wrap(
            store,
            move |_caller: Caller<'_>,
                  whandle: ResponseHandle,
                  bhandle: BodyHandle,
                  stream: i32| {
                debug!(
                    "fastly_http_resp::send_downstream whandle={} bhandle={} stream={}",
                    whandle, bhandle, stream
                );
                if stream != 0 {
                    debug!("resp_send_downstream: streaming unsupported");
                    return FastlyStatus::UNSUPPORTED.code;
                }
                let (parts, _) = clone
                    .inner
                    .borrow_mut()
                    .responses
                    .remove(whandle as usize)
                    .into_parts();
                let body = clone.inner.borrow_mut().bodies.remove(bhandle as usize);
                clone.inner.borrow_mut().response = hyper::Response::from_parts(parts, body);

                FastlyStatus::OK.code
            },
        )
    }

    fn fastly_http_req_method_get(
        &self,
        store: &Store,
    ) -> Func {
        let clone = self.clone();
        Func::wrap(
            store,
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
                match clone.inner.borrow().requests.get(handle as usize) {
                    Some(req) => {
                        debug!("fastly_http_req::method_get => {}", req.method());
                        let written = match mem
                            .write(addr as usize, req.method().as_ref().as_bytes())
                        {
                            Err(_) => {
                                return Err(Trap::new("Failed to write request HTTP method bytes"))
                            }
                            Ok(num) => num,
                        };
                        mem.write_u32(nwritten_out as usize, written as u32);
                    }
                    _ => return Err(Trap::new("Invalid body handle")),
                };

                Ok(FastlyStatus::OK.code)
            },
        )
    }

    fn fastly_http_req_method_set(
        &self,
        store: &Store,
    ) -> Func {
        let clone = self.clone();
        Func::wrap(
            store,
            move |caller: Caller<'_>, handle: RequestHandle, addr: i32, size: i32| {
                let (_, buf) = match memory!(caller).read(addr as usize, size as usize) {
                    Ok(result) => result,
                    _ => return Err(Trap::new("failed to read body memory")),
                };
                match hyper::Method::from_bytes(&buf) {
                    Ok(method) => {
                        match clone.inner.borrow_mut().requests.get_mut(handle as usize) {
                            Some(req) => *req.method_mut() = method,
                            _ => return Err(Trap::new("invalid request handler")),
                        }
                    }
                    _ => return Err(Trap::new("invalid http method")),
                };

                Ok(FastlyStatus::OK.code)
            },
        )
    }

    fn fastly_http_req_uri_get(
        &self,
        store: &Store,
    ) -> Func {
        let clone = self.clone();
        Func::wrap(
            store,
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
                match clone.inner.borrow().requests.get(handle as usize) {
                    Some(request) => {
                        let uri = request.uri().to_string();
                        debug!("fastly_http_req::uri_get => {}", uri);
                        let written = match mem.write(addr as usize, uri.as_bytes()) {
                            Err(_) => return Err(Trap::new("failed to write method bytes")),
                            Ok(num) => num,
                        };
                        mem.write_u32(nwritten_out as usize, written as u32);
                    }
                    _ => return Err(Trap::new("invalid request handle")),
                }

                Ok(FastlyStatus::OK.code)
            },
        )
    }

    fn fastly_http_req_send(
        &self,
        store: &Store,
    ) -> Func {
        let clone = self.clone();
        Func::wrap(
            store,
            move |caller: Caller<'_>,
                  req_handle: RequestHandle,
                  body_handle: BodyHandle,
                  backend_addr: i32,
                  backend_len: i32,
                  resp_handle_out: ResponseHandle,
                  resp_body_handle_out: BodyHandle| {
                debug!("fastly_http_req::send req_handle={}, body_handle={} backend_addr={} backend_len={} resp_handle_out={} resp_body_handle_out={}", req_handle, body_handle, backend_addr, backend_len, resp_handle_out, resp_body_handle_out);
                let mut memory = memory!(caller);
                let (_, buf) = match memory.read(backend_addr as usize, backend_len as usize) {
                    Ok(result) => result,
                    Err(_) => return Err(Trap::new("error reading backend name")),
                };
                let backend = std::str::from_utf8(&buf).unwrap();
                debug!("backend={}", backend);
                // todo: send it
                Ok(FastlyStatus::OK.code)
            },
        )
    }

    fn fastly_http_req_uri_set(
        &self,
        store: &Store,
    ) -> Func {
        let clone = self.clone();
        Func::wrap(
            store,
            move |caller: Caller<'_>, rhandle: RequestHandle, addr: i32, size: i32| {
                debug!("fastly_http_req::uri_set");
                match clone.inner.borrow_mut().requests.get_mut(rhandle as usize) {
                    Some(req) => {
                        let (_, buf) = match memory!(caller).read(addr as usize, size as usize) {
                            Ok(result) => result,
                            Err(_) => return Err(Trap::new("failed to read request uri")),
                        };
                        *req.uri_mut() = hyper::Uri::from_maybe_shared(buf)
                            .map_err(|_| Trap::new("invalid uri"))?;
                    }
                    _ => return Err(Trap::new("invalid request handle")),
                }
                Ok(FastlyStatus::OK.code)
            },
        )
    }

    fn fastly_http_req_cache_override_set(
        &self,
        store: &Store,
    ) -> Func {
        Func::wrap(
            store,
            move |_caller: Caller<'_>, _tag: i32, _ttl: i32, _swr: i32| {
                debug!("fastly_http_req::cache_override_set");

                FastlyStatus::OK.code
            },
        )
    }

    fn fastly_http_req_header_names_get(
        &self,
        store: &Store,
    ) -> Func {
        let clone = self.clone();
        Func::wrap(
            store,
            move |caller: Caller<'_>,
                  handle: RequestHandle,
                  addr: i32,
                  _maxlen: i32,
                  cursor: i32,
                  ending_cursor_out: i32,
                  nwritten_out: i32| {
                debug!("fastly_http_req::header_names_get");
                match clone.inner.borrow().requests.get(handle as usize) {
                    Some(req) => {
                        let mut names: Vec<_> = req.headers().keys().map(|h| h.as_str()).collect();
                        names.sort();
                        let mut memory = memory!(caller);
                        let ucursor = cursor as usize;
                        if ucursor >= names.len() {
                            memory.write_i32(nwritten_out as usize, 0);
                            memory.write_i32(ending_cursor_out as usize, -1);
                            return Ok(FastlyStatus::OK.code);
                        }
                        let mut bytes = names.get(ucursor).unwrap().as_bytes().to_vec();
                        bytes.push(0); // api requires a terminating \x00 byte
                        let written = memory.write(addr as usize, &bytes).unwrap();
                        memory.write_i32(nwritten_out as usize, written as i32);
                        memory.write_i32(
                            ending_cursor_out as usize,
                            if ucursor < names.len() - 1 {
                                cursor + 1 as i32
                            } else {
                                -1 as i32
                            },
                        );
                    }
                    _ => return Err(Trap::new("invalid request handle")),
                }

                Ok(FastlyStatus::OK.code)
            },
        )
    }

    fn fastly_http_req_header_values_get(
        &self,
        store: &Store,
    ) -> Func {
        let clone = self.clone();
        Func::wrap(
            store,
            move |caller: Caller<'_>,
                  handle: RequestHandle,
                  name_addr: i32,
                  name_size: i32,
                  addr: i32,
                  maxlen: i32,
                  cursor: i32,
                  ending_cursor_out: i32,
                  nwritten_out: i32| {
                debug!("fastly_http_req::header_values_get");

                debug!("fastly_http_req::header_names_get");
                match clone.inner.borrow().requests.get(handle as usize) {
                    Some(req) => {
                        let mut memory = memory!(caller);
                        let (_, header) = match memory.read(name_addr as usize, name_size as usize)
                        {
                            Ok(result) => result,
                            _ => return Err(Trap::new("Failed to read header name")),
                        };
                        let name = std::str::from_utf8(&header).unwrap();
                        let mut values: Vec<_> = req
                            .headers()
                            .get_all(name)
                            .into_iter()
                            .map(|h| h.as_ref())
                            .collect();
                        values.sort();
                        let mut memory = memory!(caller);
                        let ucursor = cursor as usize;
                        if ucursor >= values.len() {
                            memory.write_i32(nwritten_out as usize, 0);
                            memory.write_i32(ending_cursor_out as usize, -1);
                            return Ok(FastlyStatus::OK.code);
                        }
                        let mut bytes = values.get(ucursor).unwrap().to_vec();
                        bytes.push(0); // api requires a terminating \x00 byte
                        let written = memory.write(addr as usize, &bytes).unwrap();
                        memory.write_i32(nwritten_out as usize, written as i32);
                        memory.write_i32(
                            ending_cursor_out as usize,
                            if ucursor < values.len() - 1 {
                                cursor + 1 as i32
                            } else {
                                -1 as i32
                            },
                        );
                    }
                    _ => return Err(Trap::new("invalid request handle")),
                }

                // noop
                Ok(FastlyStatus::OK.code)
            },
        )
    }

    // bodies

    fn fastly_http_body_new(
        &self,
        store: &Store,
    ) -> Func {
        let clone = self.clone();
        Func::wrap(store, move |caller: Caller<'_>, handle_out: i32| {
            debug!("fastly_http_body::new handle_out={}", handle_out);
            let index = clone.inner.borrow().bodies.len();
            clone.inner.borrow_mut().bodies.push(Body::default());
            memory!(caller).write_u32(handle_out as usize, index as u32);

            Ok(FastlyStatus::OK.code)
        })
    }

    fn fastly_http_body_write(
        &self,
        store: &Store,
    ) -> Func {
        let clone = self.clone();
        Func::wrap(
            store,
            move |caller: Caller<'_>,
                  handle: BodyHandle,
                  addr: i32,
                  size: i32,
                  body_end: i32,
                  nwritten_out: i32| {
                debug!(
                    "fastly_http_body::write handle={} addr={} size={} body_end={} nwritten_out={}",
                    handle, addr, size, body_end, nwritten_out
                );
                match clone.inner.borrow_mut().bodies.get_mut(handle as usize) {
                    Some(body) => {
                        let mut mem = memory!(caller);
                        let (read, buf) = match mem.read(addr as usize, size as usize) {
                            Err(_) => return Err(Trap::new("Failed to read body memory")),
                            Ok((num, buf)) => (num, buf),
                        };
                        *body = Body::from(buf);

                        mem.write_u32(nwritten_out as usize, read as u32);
                    }
                    _ => return Err(Trap::new("Failed to body handle")),
                }

                Ok(FastlyStatus::OK.code)
            },
        )
    }

    // responses

    fn fastly_http_resp_status_set(
        &self,
        store: &Store,
    ) -> Func {
        let clone = self.clone();
        Func::wrap(
            store,
            move |_: Caller<'_>, whandle: ResponseHandle, status: i32| {
                debug!(
                    "fastly_http_resp::status_set whandle={} status={}",
                    whandle, status
                );

                match clone.inner.borrow_mut().responses.get_mut(whandle as usize) {
                    Some(response) => {
                        *response.status_mut() = hyper::http::StatusCode::from_u16(status as u16)
                            .map_err(|e| {
                            debug!("invalid http status");
                            wasmtime::Trap::new(e.to_string())
                        })?;
                    }
                    _ => {
                        debug!("invalid response handle");
                        return Err(wasmtime::Trap::new("invalid response handle"));
                    }
                }

                Ok(FastlyStatus::OK.code)
            },
        )
    }

    fn fastly_http_resp_new(
        &self,
        store: &Store,
    ) -> Func {
        let clone = self.clone();
        Func::wrap(store, move |caller: Caller<'_>, handle_out: i32| {
            debug!("fastly_http_resp::new handle_out={}", handle_out);
            let index = clone.inner.borrow().responses.len();
            clone.inner.borrow_mut().responses.push(Response::default());
            memory!(caller).write_u32(handle_out as usize, index as u32);

            Ok(FastlyStatus::OK.code)
        })
    }

    fn fastly_http_resp_header_values_get(
        &self,
        store: &Store,
    ) -> Func {
        Func::wrap(
            store,
            move |_caller: Caller<'_>,
                  _handle: i32,
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
        )
    }

    fn fastly_http_resp_header_values_set(
        &self,
        store: &Store,
    ) -> Func {
        Func::wrap(
            store,
            move |_handle: i32,
                  _name_addr: i32,
                  _name_size: i32,
                  _values_addr: i32,
                  _values_size: i32| {
                debug!("fastly_http_resp::header_values_set");
                FastlyStatus::OK.code
            },
        )
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
            )?
            .define(
                "fastly_http_req",
                "body_downstream_get",
                self.body_downstream_get(&store),
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
            )?
            .define(
                "fastly_http_req",
                "method_get",
                self.fastly_http_req_method_get(&store),
            )?
            .define(
                "fastly_http_req",
                "method_set",
                self.fastly_http_req_method_set(&store),
            )?.define(
            "fastly_http_req",
            "uri_get",
            self.fastly_http_req_uri_get(&store),
        )?.define(
            "fastly_http_req",
            "uri_set",
            self.fastly_http_req_uri_set(&store)
        )?.define(
            "fastly_http_req",
            "header_names_get",
            self.fastly_http_req_header_names_get(&store),
        )?.define(
            "fastly_http_req",
            "header_values_get",
            self.fastly_http_req_header_values_get(&store)
        )?.func(
            "fastly_http_req",
            "header_values_set",
            |handle: RequestHandle, name_addr: i32, name_size: i32, values_addr: i32, values_size: i32| {
                debug!("fastly_http_req::header_values_set handle={}, name_addr={} name_size={} values_addr={} values_size={}", handle, name_addr, name_size, values_addr, values_size);
                FastlyStatus::OK.code
            },
        )?.define(
            "fastly_http_req",
            "send",
            self.fastly_http_req_send(&store)
        )?.define(
            "fastly_http_req",
            "cache_override_set",
            self.fastly_http_req_cache_override_set(&store)
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
                FastlyStatus::OK.code
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
            .define("fastly_http_resp", "new", self.fastly_http_resp_new(&store))?
            .define(
                "fastly_http_resp",
                "send_downstream",
                self.fastly_http_resp_send_downstream(&store),
            )?
            .func(
                "fastly_http_resp",
                "status_get",
                self.two("fastly_http_resp::status_get"),
            )?
            .define(
                "fastly_http_resp",
                "status_set",
                self.fastly_http_resp_status_set(&store),
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
            .define(
                "fastly_http_resp",
                "header_values_get",
                self.fastly_http_resp_header_values_get(&store),
            )?
            .define(
                "fastly_http_resp",
                "header_values_set",
                self.fastly_http_resp_header_values_set(&store),
            )?;

        // body funcs

        linker
            .func(
                "fastly_http_body",
                "close",
                self.one("fastly_http_body::close"),
            )?
            .define("fastly_http_body", "new", self.fastly_http_body_new(&store))?
            .define(
                "fastly_http_body",
                "write",
                self.fastly_http_body_write(&store),
            )?
            .func("fastly_http_body", "read", || {
                debug!("fastly_http_body::read");
                FastlyStatus::OK.code
            })?
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

    fn two(
        &self,
        name: &'static str,
    ) -> impl Fn(i32, i32) -> i32 {
        move |_: i32, _: i32| {
            debug!("{} (stub)", name);
            FastlyStatus::UNSUPPORTED.code
        }
    }
}
