use crate::{
    memory,
    memory::{ReadMem, WriteMem},
    BoxError,
};
use fastly_shared::FastlyStatus;
use log::debug;
use wasmtime::{Caller, Func, Linker, Store, Trap};
use std::str;
use user_agent_parser::{Product, UserAgentParser};

lazy_static::lazy_static! {
    static ref UAP: UserAgentParser = UserAgentParser::from_str(include_str!("../uap.yaml")).expect("failed to parse uap.yaml");
}

pub fn add_to_linker<'a>(
    linker: &'a mut Linker,
    store: &Store,
) -> Result<&'a mut Linker, BoxError> {
    Ok(linker.define("fastly_uap", "parse", parse(&store))?)
}

fn parse(store: &Store) -> Func {
    Func::wrap(
        store,
        |caller: Caller<'_>,
         user_agent: i32,
         user_agent_max_len: i32,
         family_pos: i32,
         _family_max_len: i32,
         family_written: i32,
         major_pos: i32,
         _major_max_len: i32,
         major_written: i32,
         minor_pos: i32,
         _minor_max_len: i32,
         minor_written: i32,
         patch_pos: i32,
         _patch_max_len: i32,
         patch_written: i32| {
            debug!("fastly_uap::parse");
            let mut memory = memory!(caller);
            match memory.read(user_agent, user_agent_max_len) {
                Ok((_, bytes)) => match str::from_utf8(&bytes) {
                    Ok(a) => {
                        let Product {
                            name,
                            major,
                            minor,
                            patch,
                        } = UAP.parse_product(a);
                        if let Some(fam) = name {
                            match memory.write(family_pos, fam.as_bytes()) {
                                Ok(bytes) => memory.write_i32(family_written, bytes as i32),
                                _ => return Err(Trap::i32_exit(FastlyStatus::ERROR.code)),
                            }
                        }
                        if let Some(maj) = major {
                            match memory.write(major_pos, maj.as_bytes()) {
                                Ok(bytes) => memory.write_i32(major_written, bytes as i32),
                                _ => return Err(Trap::i32_exit(FastlyStatus::ERROR.code)),
                            }
                        }
                        if let Some(min) = minor {
                            match memory.write(minor_pos, min.as_bytes()) {
                                Ok(bytes) => memory.write_i32(minor_written, bytes as i32),
                                _ => return Err(Trap::i32_exit(FastlyStatus::ERROR.code)),
                            }
                        }
                        if let Some(pat) = patch {
                            match memory.write(patch_pos, pat.as_bytes()) {
                                Ok(bytes) => memory.write_i32(patch_written, bytes as i32),
                                _ => return Err(Trap::i32_exit(FastlyStatus::ERROR.code)),
                            }
                        }
                    }
                    _ => return Err(Trap::new("failed to read user agent")),
                },
                _ => return Err(Trap::new("failed to read user agent")),
            }
            Ok(FastlyStatus::OK.code)
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        tests::{body, WASM},
        Handler,
    };
    use hyper::Request;
    use std::collections::HashMap;

    #[tokio::test]
    async fn parse_works() -> Result<(), BoxError> {
        match WASM.as_ref() {
            None => Ok(()),
            Some((engine, module)) => {
                let resp = Handler::new(
                    Request::get("/uap")
                        .header("User-Agent", "curl/7.64.1")
                        .body(Default::default())?,
                )
                .run(
                    &module,
                    Store::new(&engine),
                    crate::backend::default(),
                    HashMap::default(),
                    "127.0.0.1".parse().ok(),
                )?;
                assert_eq!("curl 7 64 1", body(resp).await?);
                Ok(())
            }
        }
    }
}
