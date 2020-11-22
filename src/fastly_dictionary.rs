use crate::{
    handler::Handler,
    memory,
    memory::{ReadMem, WriteMem},
    BoxError,
};
use fastly_shared::FastlyStatus;
use log::debug;
use std::collections::HashMap;
use wasmtime::{Caller, Func, Linker, Store, Trap};

type DictionaryHandle = i32;

pub fn add_to_linker<'a>(
    linker: &'a mut Linker,
    handler: Handler,
    store: &Store,
    dictionaries: HashMap<String, HashMap<String, String>>,
) -> Result<&'a mut Linker, BoxError> {
    linker
        .define(
            "fastly_dictionary",
            "open",
            open(handler.clone(), &store, dictionaries),
        )?
        .define("fastly_dictionary", "get", get(handler, &store))?;
    Ok(linker)
}

pub fn open(
    handler: Handler,
    store: &Store,
    dictionaries: HashMap<String, HashMap<String, String>>,
) -> Func {
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
            let index = handler.inner.borrow().dictionaries.len();
            handler
                .inner
                .borrow_mut()
                .dictionaries
                .push(dictionaries.get(name).cloned().unwrap_or_default());
            memory.write_i32(dict_out, index as i32);
            Ok(FastlyStatus::OK.code)
        },
    )
}
pub fn get(
    handler: Handler,
    store: &Store,
) -> Func {
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
            match handler
                .inner
                .borrow()
                .dictionaries
                .get(dict_handle as usize)
            {
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
