use crate::{
    handler::{Endpoint, Handler},
    memory,
    memory::{ReadMem, WriteMem},
};
use fastly_shared::FastlyStatus;
use log::debug;
use wasmtime::{Caller, Func, Store, Trap};

type EndpointHandle = i32;

pub fn endpoint_get(
    handler: Handler,
    store: &Store,
) -> Func {
    Func::wrap(
        store,
        move |caller: Caller<'_>, name: i32, name_len: i32, endpoint_handle_out: i32| {
            debug!(
                "fastly_log::endpoint_get name={} name_len={} endpoint_handle_out={}",
                name, name_len, endpoint_handle_out
            );
            let mut memory = memory!(caller);
            let endpoint = match memory.read(name, name_len) {
                Ok((_, bytes)) => match std::str::from_utf8(&bytes) {
                    Ok(name) => name.to_owned(),
                    _ => return Err(Trap::new("Invalid endpoint name")),
                },
                _ => return Err(Trap::new("failed to read endpoint name")),
            };
            debug!("fastly_log::endpoint_get endpoint={}", endpoint);
            handler
                .inner
                .borrow_mut()
                .endpoints
                .push(Endpoint(endpoint));
            // todo: store handle
            memory.write_i32(endpoint_handle_out, 0);
            Ok(FastlyStatus::OK.code)
        },
    )
}

pub fn write(
    handler: Handler,
    store: &Store,
) -> Func {
    Func::wrap(
        store,
        move |caller: Caller<'_>,
              endpoint_handle: EndpointHandle,
              msg: i32,
              msg_len: i32,
              nwritten_out: i32| {
            debug!(
                "fastly_log::write endpoint_handle={} msg={} msg_len={} nwritten_out={}",
                endpoint_handle, msg, msg_len, nwritten_out
            );
            match handler
                .inner
                .borrow()
                .endpoints
                .get(endpoint_handle as usize)
            {
                Some(endpoint) => {
                    let mut memory = memory!(caller);
                    let message = match memory.read(msg, msg_len) {
                        Ok((_, bytes)) => match std::str::from_utf8(&bytes) {
                            Ok(data) => data.to_owned(),
                            _ => return Err(Trap::new("Invalid endpoint name")),
                        },
                        _ => return Err(Trap::new("failed to read endpoint name")),
                    };
                    debug!("fastly_log::write message={}", message);
                    endpoint.log(&message);
                    memory.write_i32(nwritten_out, message.len() as i32);
                }
                _ => return Err(Trap::i32_exit(FastlyStatus::BADF.code)),
            }

            Ok(FastlyStatus::OK.code)
        },
    )
}
