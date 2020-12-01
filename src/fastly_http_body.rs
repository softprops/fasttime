use crate::{
    handler::Handler,
    memory,
    memory::{ReadMem, WriteMem},
    BoxError,
};
use bytes::BytesMut;
use fastly_shared::FastlyStatus;
use log::debug;
use wasmtime::{Caller, Func, Linker, Store, Trap};

pub type BodyHandle = i32;

pub fn add_to_linker<'a>(
    linker: &'a mut Linker,
    handler: Handler,
    store: &Store,
) -> Result<&'a mut Linker, BoxError> {
    Ok(linker
        .define("fastly_http_body", "close", close(&store))?
        .define("fastly_http_body", "new", new(handler.clone(), &store))?
        .define("fastly_http_body", "write", write(handler.clone(), &store))?
        .define("fastly_http_body", "read", read(handler.clone(), &store))?
        .define("fastly_http_body", "append", append(handler, &store))?)
}

fn close(store: &Store) -> Func {
    Func::wrap(store, |_: BodyHandle| {
        debug!("fastly_http_body::close");
        // noop
        FastlyStatus::OK.code
    })
}

fn append(
    handler: Handler,
    store: &Store,
) -> Func {
    Func::wrap(
        store,
        move |dst_handle: BodyHandle, src_handle: BodyHandle| {
            debug!(
                "fastly_http_body::append dst_handle={} src_handle={}",
                dst_handle, src_handle
            );
            let src = match handler
                .inner
                .borrow_mut()
                .bodies
                .get_mut(src_handle as usize)
            {
                Some(src) => src.clone(),
                _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
            };
            match handler
                .inner
                .borrow_mut()
                .bodies
                .get_mut(dst_handle as usize)
            {
                Some(dst) => dst.extend_from_slice(src.as_ref()),
                _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
            }

            Ok(FastlyStatus::OK.code)
        },
    )
}

fn new(
    handler: Handler,
    store: &Store,
) -> Func {
    Func::wrap(store, move |caller: Caller<'_>, handle_out: i32| {
        debug!("fastly_http_body::new handle_out={}", handle_out);
        let index = handler.inner.borrow().bodies.len();
        handler.inner.borrow_mut().bodies.push(BytesMut::default());
        memory!(caller).write_u32(handle_out, index as u32);

        Ok(FastlyStatus::OK.code)
    })
}

fn write(
    handler: Handler,
    store: &Store,
) -> Func {
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
            match handler.inner.borrow_mut().bodies.get_mut(handle as usize) {
                Some(body) => {
                    let mut mem = memory!(caller);
                    let (read, buf) = match mem.read(addr, size) {
                        Ok((num, buf)) => (num, buf),
                        _ => return Err(Trap::new("Failed to read body memory")),
                    };
                    body.extend_from_slice(&buf);

                    mem.write_u32(nwritten_out, read as u32);
                }
                _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
            }

            Ok(FastlyStatus::OK.code)
        },
    )
}

fn read(
    handler: Handler,
    store: &Store,
) -> Func {
    Func::wrap(
        &store,
        move |caller: Caller<'_>,
              body_handle: BodyHandle,
              buf: i32,
              buf_len: i32,
              nread_out: i32| {
            debug!(
                "fastly_http_body::read body_handle={}, buf={} buf_len={} nread_out={}",
                body_handle, buf, buf_len, nread_out
            );
            match handler
                .inner
                .borrow_mut()
                .bodies
                .get_mut(body_handle as usize)
            {
                Some(body) => {
                    let mut memory = memory!(caller);
                    match memory.write(buf, body.as_ref()) {
                        Ok(written) => {
                            debug!("fastly_http_body::read write {} bytes", written);
                            memory.write_i32(nread_out, written as i32);
                        }
                        _ => return Err(Trap::new("failed to read body bytes")),
                    }
                }
                _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
            }

            Ok(FastlyStatus::OK.code)
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{body, WASM};
    use hyper::{Body, Request, Response};
    use std::collections::HashMap;

    #[tokio::test]
    async fn append_works() -> Result<(), BoxError> {
        match WASM.as_ref() {
            None => Ok(()),
            Some((engine, module)) => {
                let resp = Handler::new(
                    Request::get("http://127.0.0.1:3000/stream").body(Default::default())?,
                )
                .run(
                    &module,
                    Store::new(&engine),
                    Box::new(|backend: &str, _| {
                        assert_eq!("backend_name", backend);
                        Ok(Response::new(Body::from("👋")))
                    }),
                    HashMap::default(),
                    "127.0.0.1".parse().ok(),
                )?;
                assert_eq!("Welcome to Fastly Compute@Edge!Appended welcome to Fastly Compute@Edge!last line", body(resp).await?);
                Ok(())
            }
        }
    }
}
