use crate::{
    memory::{ReadMem, WriteMem},
    BoxError,
};
use fastly_shared::FastlyStatus;
use http::{request::Parts as RequestParts, response::Parts as ResponseParts};
use hyper::{Body, Request, Response};
use log::debug;
use std::{cell::RefCell, collections::HashMap, rc::Rc};
use wasmtime::{Caller, Extern, Func, Linker, Module, Store, Trap};
use wasmtime_wasi::{Wasi, WasiCtxBuilder};

type DictionaryHandle = i32;
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
    requests: Vec<RequestParts>,
    /// responses from the requests initiated within the handler
    responses: Vec<ResponseParts>,
    /// bodies created within the handler
    bodies: Vec<Body>,
    /// final handler response
    response: Response<Body>,
    /// list of loaded dictionaries
    dictionaries: Vec<HashMap<String, String>>,
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
        backends: Box<dyn crate::Backends>,
        dicionaries: HashMap<String, HashMap<String, String>>,
    ) -> Result<Response<Body>, BoxError> {
        if let Some(func) = self
            .linker(store, backends, dicionaries)?
            .instantiate(&module)?
            .get_func("_start")
        {
            func.call(&[])?;
        } else {
            return Err(Trap::new("wasm module does not define a `_start` func").into());
        }
        Ok(self.into_response())
    }

    fn fastly_dictionary_open(
        &self,
        store: &Store,
        dictionaries: HashMap<String, HashMap<String, String>>,
    ) -> Func {
        let clone = self.clone();
        Func::wrap(
            &store,
            move |caller: Caller<'_>, addr: i32, len: i32, dict_out: DictionaryHandle| {
                debug!("fastly_dictionary::open");
                let mut memory = memory!(caller);
                let (_, buf) = match memory!(caller).read(addr, len) {
                    Ok(result) => result,
                    _ => return Err(Trap::new("failed to read dictionary name")),
                };
                let name = std::str::from_utf8(&buf).unwrap();
                debug!("opening dictionary {}", name);
                let index = clone.inner.borrow().dictionaries.len();
                let dict: HashMap<String, String> =
                    dictionaries.get(name).cloned().unwrap_or_default();
                clone.inner.borrow_mut().dictionaries.push(dict);
                memory.write_i32(dict_out, index as i32);
                Ok(FastlyStatus::OK.code)
            },
        )
    }
    fn fastly_dictionary_get(
        &self,
        store: &Store,
    ) -> Func {
        let clone = self.clone();
        Func::wrap(
            &store,
            move |caller: Caller<'_>,
                  dict_handle: DictionaryHandle,
                  key_addr: i32,
                  key_len: i32,
                  value_addr: i32,
                  _value_max_len: i32,
                  nwritten: i32| {
                debug!("fastly_dictionary::get");
                match clone.inner.borrow().dictionaries.get(dict_handle as usize) {
                    Some(dict) => {
                        let mut memory = memory!(caller);
                        let (_, buf) = match memory!(caller).read(key_addr, key_len) {
                            Ok(result) => result,
                            _ => return Err(Trap::new("failed to read dictionary name")),
                        };
                        let key = std::str::from_utf8(&buf).unwrap();
                        debug!("getting dictionary key {}", key);
                        match dict.get(key) {
                            Some(value) => match memory.write(value_addr, &value.as_bytes()) {
                                Ok(written) => {
                                    memory.write_i32(nwritten, written as i32);
                                }
                                _ => return Err(Trap::new("failed to write dictionary value")),
                            },
                            _ => memory.write_i32(nwritten, 0),
                        }
                    }
                    _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
                }
                Ok(FastlyStatus::OK.code)
            },
        )
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
                debug!("fastly_http_req::body_downstream_get {:?}", parts);
                clone.inner.borrow_mut().requests.push(parts);
                clone.inner.borrow_mut().bodies.push(body);

                let mut mem = memory!(caller);
                mem.write_i32(request_handle_out, index as i32);
                mem.write_i32(body_handle_out, index as i32);
                Ok(FastlyStatus::OK.code)
            },
        )
    }

    fn fastly_http_req_new(
        &self,
        store: &Store,
    ) -> Func {
        let clone = self.clone();
        Func::wrap(store, move |caller: Caller<'_>, request: RequestHandle| {
            debug!("fastly_http_req::new request={}", request);
            let index = clone.inner.borrow().requests.len();
            let r: Request<Body> = Request::default();
            clone.inner.borrow_mut().requests.push(r.into_parts().0);
            memory!(caller).write_i32(request, index as i32);
            Ok(FastlyStatus::OK.code)
        })
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
                let parts = clone.inner.borrow_mut().responses.remove(whandle as usize);
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
                        debug!("fastly_http_req::method_get => {}", req.method);
                        let written = match mem.write(addr, req.method.as_ref().as_bytes()) {
                            Ok(num) => num,
                            _ => {
                                return Err(Trap::new("Failed to write request HTTP method bytes"))
                            }
                        };
                        mem.write_u32(nwritten_out, written as u32);
                    }
                    _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
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
                let (_, buf) = match memory!(caller).read(addr, size) {
                    Ok(result) => result,
                    _ => return Err(Trap::new("failed to read body memory")),
                };
                match hyper::Method::from_bytes(&buf) {
                    Ok(method) => {
                        match clone.inner.borrow_mut().requests.get_mut(handle as usize) {
                            Some(req) => req.method = method,
                            _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
                        }
                    }
                    _ => return Err(Trap::i32_exit(FastlyStatus::HTTPPARSE.code)),
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
                        let uri = request.uri.to_string();
                        debug!("fastly_http_req::uri_get => {}", uri);
                        let written = match mem.write(addr, uri.as_bytes()) {
                            Ok(num) => num,
                            _ => return Err(Trap::new("failed to write method bytes")),
                        };
                        mem.write_u32(nwritten_out, written as u32);
                    }
                    _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
                }

                Ok(FastlyStatus::OK.code)
            },
        )
    }

    fn fastly_http_req_send(
        &self,
        store: &Store,
        backends: Box<dyn crate::Backends>,
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
                let (_, buf) = match memory.read(backend_addr, backend_len) {
                    Ok(result) => result,
                    _ => return Err(Trap::new("error reading backend name")),
                };
                let backend = std::str::from_utf8(&buf).unwrap();
                debug!("backend={}", backend);

                let parts = clone
                    .inner
                    .borrow_mut()
                    .requests
                    .remove(req_handle as usize);
                let body = clone.inner.borrow_mut().bodies.remove(body_handle as usize);
                let req = Request::from_parts(parts, body);
                let (parts, body) = backends.send(backend, req).unwrap().into_parts();

                clone.inner.borrow_mut().responses.push(parts);
                clone.inner.borrow_mut().bodies.push(body);

                memory.write_i32(
                    resp_handle_out,
                    (clone.inner.borrow().responses.len() - 1) as i32,
                );
                memory.write_i32(
                    resp_body_handle_out,
                    (clone.inner.borrow().bodies.len() - 1) as i32,
                );

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
                debug!(
                    "fastly_http_req::uri_set rhandle={} addr={} size={}",
                    rhandle, addr, size
                );
                match clone.inner.borrow_mut().requests.get_mut(rhandle as usize) {
                    Some(req) => {
                        let (_, buf) = match memory!(caller).read(addr, size) {
                            Ok(result) => result,
                            _ => return Err(Trap::new("failed to read request uri")),
                        };
                        req.uri = hyper::Uri::from_maybe_shared(buf)
                            .map_err(|_| Trap::i32_exit(FastlyStatus::HTTPPARSE.code))?;
                    }
                    _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
                }
                Ok(FastlyStatus::OK.code)
            },
        )
    }

    fn fastly_http_req_cache_override_set(
        &self,
        store: &Store,
    ) -> Func {
        Func::wrap(store, move |tag: i32, ttl: i32, swr: i32| {
            debug!(
                "fastly_http_req::cache_override_set tag={} ttl={} swr={}",
                tag, ttl, swr
            );
            // noop
            FastlyStatus::OK.code
        })
    }

    fn fastly_http_req_cache_override_v2_set(
        &self,
        store: &Store,
    ) -> Func {
        Func::wrap(
            store,
            move |_caller: Caller<'_>,
                  handle_out: RequestHandle,
                  tag: u32,
                  ttl: u32,
                  swr: u32,
                  sk: i32, // see fastly-sys types
                  sk_len: i32| {
                debug!(
                    "fastly_http_req::cache_override_v2_set handle_out={} tag={} ttl={} swr={} sk={} sk_len={}",
                    handle_out,
                    tag,
                    ttl,
                    swr,
                    sk,
                    sk_len
                );
                // noop
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
                        let mut names: Vec<_> = req.headers.keys().map(|h| h.as_str()).collect();
                        names.sort_unstable();
                        let mut memory = memory!(caller);
                        let ucursor = cursor as usize;
                        if ucursor >= names.len() {
                            memory.write_i32(nwritten_out, 0);
                            memory.write_i32(ending_cursor_out, -1);
                            return Ok(FastlyStatus::OK.code);
                        }
                        debug!(
                            "fastly_http_req::header_names_get {:?} ({})",
                            names.get(ucursor),
                            ucursor
                        );
                        let mut bytes = names.get(ucursor).unwrap().as_bytes().to_vec();
                        bytes.push(0); // api requires a terminating \x00 byte
                        let written = memory.write(addr, &bytes).unwrap();
                        memory.write_i32(nwritten_out, written as i32);
                        memory.write_i32(
                            ending_cursor_out,
                            if ucursor < names.len() - 1 {
                                cursor + 1 as i32
                            } else {
                                -1 as i32
                            },
                        );
                    }
                    _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
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
                  _maxlen: i32,
                  cursor: i32,
                  ending_cursor_out: i32,
                  nwritten_out: i32| {
                debug!("fastly_http_req::header_values_get");
                match clone.inner.borrow().requests.get(handle as usize) {
                    Some(req) => {
                        let mut memory = memory!(caller);
                        let (_, header) = match memory.read(name_addr, name_size) {
                            Ok(result) => result,
                            _ => return Err(Trap::new("Failed to read header name")),
                        };
                        let name = std::str::from_utf8(&header).unwrap();
                        debug!("fastly_http_req::header_values_get {} ({})", name, cursor);
                        let mut values: Vec<_> = req
                            .headers
                            .get_all(name)
                            .into_iter()
                            .map(|h| h.as_ref())
                            .collect();
                        values.sort();
                        let mut memory = memory!(caller);
                        let ucursor = cursor as usize;
                        if ucursor >= values.len() {
                            memory.write_i32(nwritten_out, 0);
                            memory.write_i32(ending_cursor_out, -1);
                            return Ok(FastlyStatus::OK.code);
                        }
                        let mut bytes = values.get(ucursor).unwrap().to_vec();
                        bytes.push(0); // api requires a terminating \x00 byte
                        let written = memory.write(addr, &bytes).unwrap();
                        memory.write_i32(nwritten_out, written as i32);
                        memory.write_i32(
                            ending_cursor_out,
                            if ucursor < values.len() - 1 {
                                cursor + 1 as i32
                            } else {
                                -1 as i32
                            },
                        );
                    }
                    _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
                }

                Ok(FastlyStatus::OK.code)
            },
        )
    }

    fn fastly_http_req_version_get(
        &self,
        store: &Store,
    ) -> Func {
        let clone = self.clone();
        Func::wrap(
            store,
            move |caller: Caller<'_>, handle: RequestHandle, version_out: i32| {
                debug!(
                    "fastly_http_req::version_get handle={} version_out={}",
                    handle, version_out
                );
                match clone.inner.borrow().requests.get(handle as usize) {
                    Some(req) => memory!(caller)
                        .write_u32(version_out, crate::convert::version(req.version).as_u32()),
                    _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
                }
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
            memory!(caller).write_u32(handle_out, index as u32);

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
                        let (read, buf) = match mem.read(addr, size) {
                            Ok((num, buf)) => (num, buf),
                            _ => return Err(Trap::new("Failed to read body memory")),
                        };
                        *body = Body::from(buf);

                        mem.write_u32(nwritten_out, read as u32);
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
        Func::wrap(store, move |whandle: ResponseHandle, status: i32| {
            debug!(
                "fastly_http_resp::status_set whandle={} status={}",
                whandle, status
            );

            match clone.inner.borrow_mut().responses.get_mut(whandle as usize) {
                Some(response) => {
                    response.status =
                        hyper::http::StatusCode::from_u16(status as u16).map_err(|_| {
                            debug!("invalid http status");
                            Trap::i32_exit(FastlyStatus::HTTPPARSE.code)
                        })?;
                }
                _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
            }

            Ok(FastlyStatus::OK.code)
        })
    }

    fn fastly_http_resp_new(
        &self,
        store: &Store,
    ) -> Func {
        let clone = self.clone();
        Func::wrap(store, move |caller: Caller<'_>, handle_out: i32| {
            debug!("fastly_http_resp::new handle_out={}", handle_out);
            let index = clone.inner.borrow().responses.len();
            let resp: Response<Body> = Response::default();
            clone.inner.borrow_mut().responses.push(resp.into_parts().0);
            memory!(caller).write_u32(handle_out, index as u32);

            Ok(FastlyStatus::OK.code)
        })
    }

    fn fastly_http_resp_header_values_get(
        &self,
        store: &Store,
    ) -> Func {
        let clone = self.clone();
        Func::wrap(
            store,
            move |caller: Caller<'_>,
                  handle: ResponseHandle,
                  name_addr: i32,
                  name_size: i32,
                  addr: i32,
                  _maxlen: i32,
                  cursor: i32,
                  ending_cursor_out: i32,
                  nwritten_out: i32| {
                debug!("fastly_http_resp::header_values_get");

                let mut memory = memory!(caller);
                match clone.inner.borrow_mut().responses.get_mut(handle as usize) {
                    Some(resp) => {
                        let name = match memory.read(name_addr, name_size) {
                            Ok((_, bytes)) => {
                                hyper::header::HeaderName::from_bytes(&bytes).unwrap()
                            }
                            _ => return Err(Trap::new("Failed to read header name")),
                        };

                        let mut values: Vec<_> = resp
                            .headers
                            .get_all(name)
                            .into_iter()
                            .map(|e| e.as_ref())
                            .collect();
                        values.sort();

                        let ucursor = cursor as usize;
                        if ucursor >= values.len() {
                            memory.write_i32(nwritten_out, 0);
                            memory.write_i32(ending_cursor_out, -1);
                            return Ok(FastlyStatus::OK.code);
                        }
                        let mut bytes = values.get(ucursor).unwrap().to_vec();
                        bytes.push(0); // api requires a terminating \x00 byte
                        let written = memory.write(addr, &bytes).unwrap();
                        memory.write_i32(nwritten_out, written as i32);
                        memory.write_i32(
                            ending_cursor_out,
                            if ucursor < values.len() - 1 {
                                cursor + 1 as i32
                            } else {
                                -1 as i32
                            },
                        );
                    }
                    _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
                }

                Ok(FastlyStatus::OK.code)
            },
        )
    }

    fn fastly_http_resp_header_values_set(
        &self,
        store: &Store,
    ) -> Func {
        let clone = self.clone();
        Func::wrap(
            store,
            move |caller: Caller<'_>,
                  handle: ResponseHandle,
                  name_addr: i32,
                  name_size: i32,
                  values_addr: i32,
                  values_size: i32| {
                debug!("fastly_http_resp::header_values_set");
                let mut memory = memory!(caller);
                match clone.inner.borrow_mut().responses.get_mut(handle as usize) {
                    Some(resp) => {
                        let name = match memory.read(name_addr, name_size) {
                            Ok((_, bytes)) => {
                                hyper::header::HeaderName::from_bytes(&bytes).unwrap()
                            }
                            _ => return Err(Trap::new("Failed to read header name")),
                        };

                        let value = match memory.read(values_addr, values_size) {
                            Ok((_, bytes)) => {
                                hyper::header::HeaderValue::from_bytes(&bytes).unwrap()
                            }
                            _ => return Err(Trap::new("Failed to read header name")),
                        };
                        resp.headers.append(name, value);
                    }
                    _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
                }

                Ok(FastlyStatus::OK.code)
            },
        )
    }

    fn fastly_http_resp_status_get(
        &self,
        store: &Store,
    ) -> Func {
        let clone = self.clone();
        Func::wrap(
            store,
            move |caller: Caller<'_>, resp_handle: ResponseHandle, status: i32| {
                debug!(
                    "fastly_http_resp::status_get resp_handle={} status={}",
                    resp_handle, status
                );
                match clone.inner.borrow().responses.get(resp_handle as usize) {
                    Some(resp) => memory!(caller).write_i32(status, resp.status.as_u16() as i32),
                    _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
                }
                Ok(FastlyStatus::OK.code)
            },
        )
    }

    fn fastly_http_resp_version_get(
        &self,
        store: &Store,
    ) -> Func {
        let clone = self.clone();
        Func::wrap(
            store,
            move |caller: Caller<'_>, resp_handle: ResponseHandle, version_out: i32| {
                debug!(
                    "fastly_http_resp::version_get resp_handle={} version={}",
                    resp_handle, version_out
                );
                match clone.inner.borrow().responses.get(resp_handle as usize) {
                    Some(resp) => memory!(caller)
                        .write_u32(version_out, crate::convert::version(resp.version).as_u32()),
                    _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
                }

                Ok(FastlyStatus::OK.code)
            },
        )
    }

    fn fastly_http_resp_version_set(
        &self,
        store: &Store,
    ) -> Func {
        Func::wrap(store, move |whandle: ResponseHandle, version: i32| {
            debug!(
                "fastly_http_resp::version_set handle={} version={}",
                whandle, version
            );
            Ok(FastlyStatus::OK.code)
        })
    }

    /// Builds a new linker given a provided `Store`
    /// configured with WASI and Fastly sys func implementations
    fn linker(
        &mut self,
        store: Store,
        backends: Box<dyn crate::Backends>,
        dictionaries: HashMap<String, HashMap<String, String>>,
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

        linker
            .define(
                "fastly_dictionary",
                "open",
                self.fastly_dictionary_open(&store, dictionaries),
            )?
            .define(
                "fastly_dictionary",
                "get",
                self.fastly_dictionary_get(&store),
            )?;

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
            .define("fastly_http_req", "new", self.fastly_http_req_new(&store))?
            .define(
                "fastly_http_req",
                "version_get",
                self.fastly_http_req_version_get(&store)
            )?
            .func(
                "fastly_http_req",
                "version_set",
                move |handle: RequestHandle, version_out: i32| {
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
            self.fastly_http_req_send(&store, backends)
        )?.define(
            "fastly_http_req",
            "cache_override_set",
            self.fastly_http_req_cache_override_set(&store)
        )?.define(
            "fastly_http_req",
            "cache_override_v2_set",
            self.fastly_http_req_cache_override_v2_set(&store)
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
            .define(
                "fastly_http_resp",
                "status_get",
                self.fastly_http_resp_status_get(&store),
            )?
            .define(
                "fastly_http_resp",
                "status_set",
                self.fastly_http_resp_status_set(&store),
            )?
            .define(
                "fastly_http_resp",
                "version_get",
                self.fastly_http_resp_version_get(&store),
            )?
            .define(
                "fastly_http_resp",
                "version_set",
                self.fastly_http_resp_version_set(&store),
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
