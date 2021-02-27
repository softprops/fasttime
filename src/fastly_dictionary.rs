use crate::{
    handler::Handler,
    memory,
    memory::{ReadMem, WriteMem},
    BoxError,
};
use fastly_shared::FastlyStatus;
use log::debug;
use std::{collections::HashMap, str};
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

fn open(
    handler: Handler,
    store: &Store,
    dictionaries: HashMap<String, HashMap<String, String>>,
) -> Func {
    Func::wrap(
        &store,
        move |caller: Caller<'_>, addr: i32, len: i32, dict_out: DictionaryHandle| {
            debug!(
                "fastly_dictionary::open addr={} len={} dict_out={}",
                addr, len, dict_out
            );
            let mut memory = memory!(caller);
            let (_, buf) = match memory.read_bytes(addr, len) {
                Ok(result) => result,
                _ => return Err(Trap::new("failed to read dictionary name")),
            };
            let name = str::from_utf8(&buf).expect("utf8");
            match dictionaries.get(name) {
                Some(dict) => {
                    debug!("fastly_dictionary::open opening dictionary {}", name);
                    let index = handler.inner.borrow().dictionaries.len();
                    handler.inner.borrow_mut().dictionaries.push(dict.clone());
                    memory.write_i32(dict_out, index as i32);
                    Ok(FastlyStatus::OK.code)
                }
                _ => {
                    debug!("fastly_dictionary::open no dictionary named {}", name);
                    Err(Trap::i32_exit(FastlyStatus::INVAL.code))
                }
            }
        },
    )
}

fn get(
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
                    let (_, buf) = match memory!(caller).read_bytes(key_addr, key_len) {
                        Ok(result) => result,
                        _ => return Err(Trap::new("failed to read dictionary name")),
                    };
                    let key = str::from_utf8(&buf).unwrap();
                    debug!("getting dictionary key {}", key);
                    match dict.get(key) {
                        Some(value) => match memory.write_bytes(value_addr, &value.as_bytes()) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{body, WASM};
    use hyper::Request;

    #[tokio::test]
    async fn hits_work() -> Result<(), BoxError> {
        match WASM.as_ref() {
            None => Ok(()),
            Some((engine, module)) => {
                let mut dictionaries = HashMap::new();
                let mut dictionary = HashMap::new();
                dictionary.insert("foo".to_string(), "bar".to_string());
                dictionaries.insert("dict".to_string(), dictionary);
                let resp = Handler::new(Request::get("/dictionary-hit").body(Default::default())?)
                    .run(
                        &module,
                        Store::new(&engine),
                        crate::backend::default(),
                        dictionaries,
                        "127.0.0.1".parse().ok(),
                    )?;
                assert_eq!("dict::foo is bar", body(resp).await?);
                Ok(())
            }
        }
    }

    #[tokio::test]
    #[ignore]
    async fn misses_work() -> Result<(), BoxError> {
        match WASM.as_ref() {
            None => Ok(()),
            Some((engine, module)) => {
                match Handler::new(Request::get("/dictionary-miss").body(Default::default())?).run(
                    &module,
                    Store::new(&engine),
                    crate::backend::default(),
                    HashMap::default(),
                    "127.0.0.1".parse().ok(),
                ) {
                    Ok(_) => panic!("expected error"),
                    Err(e) => assert_eq!(e.to_string(), "test"),
                }
                Ok(())
            }
        }
    }
}
