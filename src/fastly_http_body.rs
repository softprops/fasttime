use crate::{
    handler::Handler,
    memory,
    memory::{ReadMem, WriteMem},
    BoxError,
};
use fastly_shared::FastlyStatus;
use hyper::Body;
use log::debug;
use wasmtime::{Caller, Func, Linker, Store, Trap};

pub type BodyHandle = i32;

pub fn add_to_linker<'a>(
    linker: &'a mut Linker,
    handler: Handler,
    store: &Store,
) -> Result<&'a mut Linker, BoxError> {
    Ok(linker
        .func("fastly_http_body", "close", || {
            debug!("fastly_http_body::close");
            FastlyStatus::OK.code
        })?
        .define(
            "fastly_http_body",
            "new",
            crate::fastly_http_body::new(handler.clone(), &store),
        )?
        .define(
            "fastly_http_body",
            "write",
            crate::fastly_http_body::write(handler.clone(), &store),
        )?
        .define(
            "fastly_http_body",
            "read",
            crate::fastly_http_body::read(handler, &store),
        )?
        .func("fastly_http_body", "append", || {
            debug!("fastly_http_body::append");
            FastlyStatus::OK.code
        })?)
}

fn new(
    handler: Handler,
    store: &Store,
) -> Func {
    Func::wrap(store, move |caller: Caller<'_>, handle_out: i32| {
        debug!("fastly_http_body::new handle_out={}", handle_out);
        let index = handler.inner.borrow().bodies.len();
        handler.inner.borrow_mut().bodies.push(Body::default());
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
                    *body = Body::from(buf);

                    mem.write_u32(nwritten_out, read as u32);
                }
                _ => return Err(Trap::new("Failed to body handle")),
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
                    match memory.write(
                        buf,
                        futures_executor::block_on(hyper::body::to_bytes(body))
                            .unwrap()
                            .as_ref(),
                    ) {
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
