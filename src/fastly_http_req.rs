use crate::{
    backend::Backends,
    fastly_http_body::BodyHandle,
    fastly_http_resp::ResponseHandle,
    geo,
    handler::Handler,
    memory,
    memory::{ReadMem, WriteMem},
};
use fastly_shared::{FastlyStatus, HttpVersion};
use hyper::{
    header::{HeaderName, HeaderValue},
    Body, Method, Request, Uri,
};
use log::debug;
use std::{convert::TryFrom, net::IpAddr};
use wasmtime::{Caller, Func, Store, Trap};

pub type RequestHandle = i32;

pub fn original_header_names_get(
    handler: Handler,
    store: &Store,
) -> Func {
    Func::wrap(
        store,
        move |caller: Caller<'_>,
              buf: i32,
              _buf_len: i32,
              cursor: i32,
              ending_cursor: i32,
              nwritten: i32| {
            debug!("fastly_http_req::original_header_names_get");

            let mut names: Vec<_> = handler
                .inner
                .borrow()
                .request
                .as_ref()
                .map(|r| {
                    r.headers()
                        .keys()
                        .map(HeaderName::as_str)
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                })
                .or_else(|| {
                    handler.inner.borrow().requests.first().map(|r| {
                        r.headers
                            .keys()
                            .map(HeaderName::as_str)
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                    })
                })
                .unwrap_or_default();

            names.sort_unstable();
            let mut memory = memory!(caller);
            let ucursor = cursor as usize;
            if ucursor >= names.len() {
                memory.write_i32(nwritten, 0);
                memory.write_i32(ending_cursor, -1);
                return Ok(FastlyStatus::OK.code);
            }
            debug!(
                "fastly_http_req::header_names_get {:?} ({})",
                names.get(ucursor),
                ucursor
            );
            let mut bytes = names.get(ucursor).unwrap().as_bytes().to_vec();
            bytes.push(0); // api requires a terminating \x00 byte
            let written = memory.write(buf, &bytes).unwrap();
            memory.write_i32(nwritten, written as i32);
            memory.write_i32(
                ending_cursor,
                if ucursor < names.len() - 1 {
                    cursor + 1 as i32
                } else {
                    -1 as i32
                },
            );

            Ok(FastlyStatus::OK.code)
        },
    )
}

pub fn original_header_count(
    handler: Handler,
    store: &Store,
) -> Func {
    Func::wrap(store, move |caller: Caller<'_>, count_out: i32| {
        debug!(
            "fastly_http_req::original_header_count count_out={}",
            count_out
        );
        let count = handler
            .inner
            .borrow()
            .request
            .as_ref()
            .map(|r| r.headers().len())
            .or_else(|| {
                handler
                    .inner
                    .borrow()
                    .requests
                    .first()
                    .map(|r| r.headers.len())
            })
            .unwrap_or_default();
        memory!(caller).write_i32(count_out, count as i32);
        Ok(FastlyStatus::OK.code)
    })
}

pub fn body_downstream_get(
    handler: Handler,
    store: &Store,
) -> Func {
    Func::wrap(
        &store,
        move |caller: Caller<'_>, request_handle_out: RequestHandle, body_handle_out: i32| {
            debug!(
                "fastly_http_req::body_downstream_get request_handle_out={} body_handle_out={}",
                request_handle_out, body_handle_out
            );
            let index = handler.inner.borrow().requests.len();
            let (parts, body) = handler
                .inner
                .borrow_mut()
                .request
                .take()
                .unwrap()
                .into_parts();
            debug!("fastly_http_req::body_downstream_get {:?}", parts);
            handler.inner.borrow_mut().requests.push(parts);
            handler.inner.borrow_mut().bodies.push(body);

            let mut mem = memory!(caller);
            mem.write_i32(request_handle_out, index as i32);
            mem.write_i32(body_handle_out, index as i32);
            Ok(FastlyStatus::OK.code)
        },
    )
}

pub fn downstream_client_ip_addr(
    _handler: Handler,
    store: &Store,
    ip: IpAddr,
) -> Func {
    Func::wrap(
        &store,
        move |caller: Caller<'_>, addr: i32, num_written: i32| {
            let mut memory = memory!(caller);
            debug!(
                "fastly_http_req::downstream_client_ip_addr addr={} num_written={}",
                addr, num_written
            );
            debug!(
                "fastly_http_req::downstream_client_ip_addr => {}",
                ip.to_string()
            );
            let bytes = match ip {
                IpAddr::V4(ip) => ip.octets().to_vec(),
                IpAddr::V6(ip) => ip.octets().to_vec(),
            };
            match memory.write(addr, &bytes) {
                Ok(written) => memory.write_i32(num_written, written as i32),
                _ => return Err(Trap::new("failed to write ip address")),
            }
            Ok(FastlyStatus::OK.code)
        },
    )
}

pub fn new(
    handler: Handler,
    store: &Store,
) -> Func {
    Func::wrap(store, move |caller: Caller<'_>, request: RequestHandle| {
        debug!("fastly_http_req::new request={}", request);
        let index = handler.inner.borrow().requests.len();
        let r: Request<Body> = Request::default();
        handler.inner.borrow_mut().requests.push(r.into_parts().0);
        memory!(caller).write_i32(request, index as i32);
        Ok(FastlyStatus::OK.code)
    })
}

pub fn method_get(
    handler: Handler,
    store: &Store,
) -> Func {
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
            match handler.inner.borrow().requests.get(handle as usize) {
                Some(req) => {
                    debug!("fastly_http_req::method_get => {}", req.method);
                    let written = match mem.write(addr, req.method.as_ref().as_bytes()) {
                        Ok(num) => num,
                        _ => return Err(Trap::new("Failed to write request HTTP method bytes")),
                    };
                    mem.write_u32(nwritten_out, written as u32);
                }
                _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
            };

            Ok(FastlyStatus::OK.code)
        },
    )
}

pub fn method_set(
    handler: Handler,
    store: &Store,
) -> Func {
    Func::wrap(
        store,
        move |caller: Caller<'_>, handle: RequestHandle, addr: i32, size: i32| {
            let (_, buf) = match memory!(caller).read(addr, size) {
                Ok(result) => result,
                _ => return Err(Trap::new("failed to read body memory")),
            };
            match Method::from_bytes(&buf) {
                Ok(method) => match handler.inner.borrow_mut().requests.get_mut(handle as usize) {
                    Some(req) => req.method = method,
                    _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
                },
                _ => return Err(Trap::i32_exit(FastlyStatus::HTTPPARSE.code)),
            };

            Ok(FastlyStatus::OK.code)
        },
    )
}

pub fn uri_get(
    handler: Handler,
    store: &Store,
) -> Func {
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
            match handler.inner.borrow().requests.get(handle as usize) {
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

pub fn send(
    handler: Handler,
    store: &Store,
    backends: Box<dyn crate::Backends>,
) -> Func {
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

            let parts = handler
                .inner
                .borrow_mut()
                .requests
                .remove(req_handle as usize);
            let body = handler
                .inner
                .borrow_mut()
                .bodies
                .remove(body_handle as usize);
            let req = Request::from_parts(parts, body);
            let (parts, body) = match backend {
                "geolocation" => geo::GeoBackend(Box::new(geo::Geo::default()))
                    .send(backend, req)
                    .expect("failed to send request")
                    .into_parts(),
                other => backends
                    .send(other, req)
                    .expect("failed to send request")
                    .into_parts(),
            };

            handler.inner.borrow_mut().responses.push(parts);
            handler.inner.borrow_mut().bodies.push(body);

            memory.write_i32(
                resp_handle_out,
                (handler.inner.borrow().responses.len() - 1) as i32,
            );
            memory.write_i32(
                resp_body_handle_out,
                (handler.inner.borrow().bodies.len() - 1) as i32,
            );

            Ok(FastlyStatus::OK.code)
        },
    )
}

pub fn uri_set(
    handler: Handler,
    store: &Store,
) -> Func {
    Func::wrap(
        store,
        move |caller: Caller<'_>, rhandle: RequestHandle, addr: i32, size: i32| {
            debug!(
                "fastly_http_req::uri_set rhandle={} addr={} size={}",
                rhandle, addr, size
            );
            match handler
                .inner
                .borrow_mut()
                .requests
                .get_mut(rhandle as usize)
            {
                Some(req) => {
                    let (_, buf) = match memory!(caller).read(addr, size) {
                        Ok(result) => result,
                        _ => return Err(Trap::new("failed to read request uri")),
                    };
                    req.uri = Uri::from_maybe_shared(buf)
                        .map_err(|_| Trap::i32_exit(FastlyStatus::HTTPPARSE.code))?;
                }
                _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
            }
            Ok(FastlyStatus::OK.code)
        },
    )
}

pub fn cache_override_set(
    _handler: Handler,
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

pub fn cache_override_v2_set(
    _handler: Handler,
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

pub fn header_names_get(
    handler: Handler,
    store: &Store,
) -> Func {
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
            match handler.inner.borrow().requests.get(handle as usize) {
                Some(req) => {
                    let mut names: Vec<_> = req.headers.keys().map(HeaderName::as_str).collect();
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

pub fn header_values_get(
    handler: Handler,
    store: &Store,
) -> Func {
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
            match handler.inner.borrow_mut().requests.get_mut(handle as usize) {
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

pub fn header_values_set(
    handler: Handler,
    store: &Store,
) -> Func {
    Func::wrap(
        &store,
        move |caller: Caller<'_>,
              handle: RequestHandle,
              name_addr: i32,
              name_size: i32,
              values_addr: i32,
              values_size: i32| {
            debug!("fastly_http_req::header_values_set handle={}, name_addr={} name_size={} values_addr={} values_size={}", handle, name_addr, name_size, values_addr, values_size);
            match handler.inner.borrow_mut().requests.get_mut(handle as usize) {
                Some(req) => {
                    let mut memory = memory!(caller);
                    let name = match memory.read(name_addr, name_size) {
                        Ok((_, bytes)) => match HeaderName::from_bytes(&bytes) {
                            Ok(name) => name,
                            _ => {
                                return Err(Trap::new(format!(
                                    "invalid header name {:?}",
                                    std::str::from_utf8(&bytes)
                                )))
                            }
                        },
                        _ => return Err(Trap::new("failed to read header name")),
                    };
                    // values are \u{0} terminated so read 1 less byte
                    let value = match memory.read(values_addr, values_size - 1) {
                        Ok((_, bytes)) => match HeaderValue::from_bytes(&bytes) {
                            Ok(value) => value,
                            _ => {
                                return Err(Trap::new(format!(
                                    "invalid header value for header '{}' {:?}",
                                    name,
                                    std::str::from_utf8(&bytes)
                                )))
                            }
                        },
                        _ => return Err(Trap::new("failed to read header value")),
                    };
                    req.headers.append(name, value);
                }
                _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
            }
            Ok(FastlyStatus::OK.code)
        },
    )
}

pub fn version_get(
    handler: Handler,
    store: &Store,
) -> Func {
    Func::wrap(
        store,
        move |caller: Caller<'_>, handle: RequestHandle, version_out: i32| {
            debug!(
                "fastly_http_req::version_get handle={} version_out={}",
                handle, version_out
            );
            match handler.inner.borrow().requests.get(handle as usize) {
                Some(req) => {
                    memory!(caller).write_u32(version_out, HttpVersion::from(req.version).as_u32())
                }
                _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
            }
            Ok(FastlyStatus::OK.code)
        },
    )
}

pub fn version_set(
    handler: Handler,
    store: &Store,
) -> Func {
    Func::wrap(&store, move |handle: RequestHandle, version: i32| {
        debug!(
            "fastly_http_req::version_set handle={} version={}",
            handle, version
        );
        match handler.inner.borrow_mut().requests.get_mut(handle as usize) {
            Some(req) => {
                req.version = HttpVersion::try_from(version as u32)
                    .expect("invalid version")
                    .into();
            }
            _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
        }

        Ok(FastlyStatus::OK.code)
    })
}
