use crate::{
    fastly_http_body::BodyHandle,
    handler::Handler,
    memory,
    memory::{ReadMem, WriteMem},
    BoxError,
};
use fastly_shared::{FastlyStatus, HttpVersion};
use hyper::{
    header::{HeaderName, HeaderValue},
    Body, Response, StatusCode,
};
use log::debug;
use std::{convert::TryFrom, str};
use wasmtime::{Caller, Func, Linker, Store, Trap};

pub type ResponseHandle = i32;

pub fn add_to_linker<'a>(
    linker: &'a mut Linker,
    handler: Handler,
    store: &Store,
) -> Result<&'a mut Linker, BoxError> {
    Ok(linker
        .define("fastly_http_resp", "new", new(handler.clone(), &store))?
        .define(
            "fastly_http_resp",
            "send_downstream",
            send_downstream(handler.clone(), &store),
        )?
        .define(
            "fastly_http_resp",
            "status_get",
            status_get(handler.clone(), &store),
        )?
        .define(
            "fastly_http_resp",
            "status_set",
            status_set(handler.clone(), &store),
        )?
        .define(
            "fastly_http_resp",
            "version_get",
            version_get(handler.clone(), &store),
        )?
        .define(
            "fastly_http_resp",
            "version_set",
            version_set(handler.clone(), &store),
        )?
        .define(
            "fastly_http_resp",
            "header_names_get",
            header_names_get(handler.clone(), &store),
        )?
        .define(
            "fastly_http_resp",
            "header_values_get",
            header_values_get(handler.clone(), &store),
        )?
        .define(
            "fastly_http_resp",
            "header_values_set",
            header_values_set(handler, &store),
        )?)
}

fn send_downstream(
    handler: Handler,
    store: &Store,
) -> Func {
    Func::wrap(
        store,
        move |_caller: Caller<'_>, whandle: ResponseHandle, bhandle: BodyHandle, stream: i32| {
            debug!(
                "fastly_http_resp::send_downstream whandle={} bhandle={} stream={}",
                whandle, bhandle, stream
            );
            if stream != 0 {
                debug!("resp_send_downstream: streaming unsupported");
                return FastlyStatus::UNSUPPORTED.code;
            }
            let parts = handler
                .inner
                .borrow_mut()
                .responses
                .remove(whandle as usize);
            let body = handler.inner.borrow_mut().bodies.remove(bhandle as usize);
            handler.inner.borrow_mut().response =
                Response::from_parts(parts, Body::from(body.to_vec()));

            FastlyStatus::OK.code
        },
    )
}

fn status_set(
    handler: Handler,
    store: &Store,
) -> Func {
    Func::wrap(store, move |whandle: ResponseHandle, status: i32| {
        debug!(
            "fastly_http_resp::status_set whandle={} status={}",
            whandle, status
        );

        match handler
            .inner
            .borrow_mut()
            .responses
            .get_mut(whandle as usize)
        {
            Some(response) => {
                response.status = StatusCode::from_u16(status as u16).map_err(|_| {
                    debug!("invalid http status");
                    Trap::i32_exit(FastlyStatus::HTTPPARSE.code)
                })?;
            }
            _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
        }

        Ok(FastlyStatus::OK.code)
    })
}

fn new(
    handler: Handler,
    store: &Store,
) -> Func {
    Func::wrap(store, move |caller: Caller<'_>, handle_out: i32| {
        debug!("fastly_http_resp::new handle_out={}", handle_out);
        let index = handler.inner.borrow().responses.len();
        let resp: Response<Body> = Response::default();
        handler
            .inner
            .borrow_mut()
            .responses
            .push(resp.into_parts().0);
        memory!(caller).write_u32(handle_out, index as u32);

        Ok(FastlyStatus::OK.code)
    })
}

fn header_names_get(
    handler: Handler,
    store: &Store,
) -> Func {
    Func::wrap(
        &store,
        move |caller: Caller<'_>,
              handle: ResponseHandle,
              addr: i32,
              maxlen: i32,
              cursor: i32,
              ending_cursor_out: i32,
              nwritten_out: i32| {
            debug!("fastly_http_resp::header_names_get handle={} addr={} maxlen={} cursor={} ending_cursor_out={} nwritten_out={}",
        handle, addr, maxlen, cursor, ending_cursor_out, nwritten_out);
            match handler.inner.borrow().responses.get(handle as usize) {
                Some(resp) => {
                    let mut names: Vec<_> = resp.headers.keys().map(HeaderName::as_str).collect();
                    names.sort_unstable();
                    let mut memory = memory!(caller);
                    let ucursor = cursor as usize;
                    match names.get(ucursor) {
                        Some(hdr) => {
                            let mut bytes = hdr.as_bytes().to_vec();
                            bytes.push(0); // api requires a terminating \x00 byte
                            let written = memory.write(addr, &bytes).unwrap();
                            memory.write_i32(nwritten_out, written as i32);
                            memory.write_i32(
                                ending_cursor_out,
                                if ucursor < names.len() - 1 {
                                    cursor + 1_i32
                                } else {
                                    -1_i32
                                },
                            );
                        }
                        _ => {
                            memory.write_i32(nwritten_out, 0);
                            memory.write_i32(ending_cursor_out, -1);
                            return Ok(FastlyStatus::OK.code);
                        }
                    }
                }
                _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
            }

            Ok(FastlyStatus::OK.code)
        },
    )
}

fn header_values_get(
    handler: Handler,
    store: &Store,
) -> Func {
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
            match handler
                .inner
                .borrow_mut()
                .responses
                .get_mut(handle as usize)
            {
                Some(resp) => {
                    let name = match memory.read(name_addr, name_size) {
                        Ok((_, bytes)) => HeaderName::from_bytes(&bytes).unwrap(),
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
                    match values.get(ucursor) {
                        Some(val) => {
                            let mut bytes = val.to_vec();
                            bytes.push(0); // api requires a terminating \x00 byte
                            let written = memory.write(addr, &bytes).unwrap();
                            memory.write_i32(nwritten_out, written as i32);
                            memory.write_i32(
                                ending_cursor_out,
                                if ucursor < values.len() - 1 {
                                    cursor + 1_i32
                                } else {
                                    -1_i32
                                },
                            );
                        }
                        _ => {
                            memory.write_i32(nwritten_out, 0);
                            memory.write_i32(ending_cursor_out, -1);
                            return Ok(FastlyStatus::OK.code);
                        }
                    }
                }
                _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
            }

            Ok(FastlyStatus::OK.code)
        },
    )
}

fn header_values_set(
    handler: Handler,
    store: &Store,
) -> Func {
    Func::wrap(
        store,
        move |caller: Caller<'_>,
              handle: ResponseHandle,
              name_addr: i32,
              name_size: i32,
              values_addr: i32,
              values_size: i32| {
            debug!("fastly_http_resp::header_values_set handle={} name_addr={} name_size={} value_addr={} value_size={}", 
            handle, name_addr, name_size, values_addr, values_size);
            let mut memory = memory!(caller);
            match handler
                .inner
                .borrow_mut()
                .responses
                .get_mut(handle as usize)
            {
                Some(resp) => {
                    let name = match memory.read(name_addr, name_size) {
                        Ok((_, bytes)) => match HeaderName::from_bytes(&bytes) {
                            Ok(name) => name,
                            _ => {
                                return Err(Trap::new(format!(
                                    "Invalid header name {:?}",
                                    str::from_utf8(&bytes)
                                )))
                            }
                        },
                        _ => return Err(Trap::new("Failed to read header name")),
                    };
                    // values are \u{0} terminated so read one less byte
                    let value = match memory.read(values_addr, values_size - 1) {
                        Ok((_, bytes)) => match HeaderValue::from_bytes(&bytes) {
                            Ok(value) => value,
                            _ => {
                                return Err(Trap::new(format!(
                                    "Invalid header value for header {} {:?}",
                                    name,
                                    str::from_utf8(&bytes)
                                )))
                            }
                        },
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

fn status_get(
    handler: Handler,
    store: &Store,
) -> Func {
    Func::wrap(
        store,
        move |caller: Caller<'_>, resp_handle: ResponseHandle, status: i32| {
            debug!(
                "fastly_http_resp::status_get resp_handle={} status={}",
                resp_handle, status
            );
            match handler.inner.borrow().responses.get(resp_handle as usize) {
                Some(resp) => memory!(caller).write_i32(status, resp.status.as_u16() as i32),
                _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
            }
            Ok(FastlyStatus::OK.code)
        },
    )
}

fn version_get(
    handler: Handler,
    store: &Store,
) -> Func {
    Func::wrap(
        store,
        move |caller: Caller<'_>, resp_handle: ResponseHandle, version_out: i32| {
            debug!(
                "fastly_http_resp::version_get resp_handle={} version={}",
                resp_handle, version_out
            );
            match handler.inner.borrow().responses.get(resp_handle as usize) {
                Some(resp) => {
                    memory!(caller).write_u32(version_out, HttpVersion::from(resp.version).as_u32())
                }
                _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
            }

            Ok(FastlyStatus::OK.code)
        },
    )
}

fn version_set(
    handler: Handler,
    store: &Store,
) -> Func {
    Func::wrap(store, move |whandle: ResponseHandle, version: i32| {
        debug!(
            "fastly_http_resp::version_set handle={} version={}",
            whandle, version
        );
        match handler
            .inner
            .borrow_mut()
            .responses
            .get_mut(whandle as usize)
        {
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
