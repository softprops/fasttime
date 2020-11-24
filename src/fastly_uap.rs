use crate::{
    memory,
    memory::{ReadMem, WriteMem},
    BoxError,
};
use fastly_shared::FastlyStatus;
use log::debug;
use wasmtime::{Caller, Func, Linker, Store, Trap};
use woothee::parser::{Parser, WootheeResult};

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
         family: i32,
         _family_max_len: i32,
         family_written: i32,
         major: i32,
         _major_max_len: i32,
         major_written: i32,
         minor: i32,
         _minor_max_len: i32,
         minor_written: i32,
         patch: i32,
         _patch_max_len: i32,
         patch_written: i32| {
            debug!("fastly_uap::parse");
            let mut memory = memory!(caller);
            match memory.read(user_agent, user_agent_max_len) {
                Ok((_, bytes)) => match std::str::from_utf8(&bytes) {
                    Ok(a) => {
                        let (fam, (maj, min, pat)) = match Parser::new().parse(a) {
                            Some(WootheeResult {
                                category, version, ..
                            }) => (
                                category,
                                match version.split('.').collect::<Vec<_>>()[..] {
                                    [maj, min, pat] => (maj, min, pat),
                                    [maj, min] => (maj, min, ""),
                                    [maj] => (maj, "", ""),
                                    _ => ("0", "0", "0"),
                                },
                            ),
                            _ => ("", ("0", "0", "0")),
                        };
                        match memory.write(family, fam.as_bytes()) {
                            Ok(bytes) => memory.write_i32(family_written, bytes as i32),
                            _ => return Err(Trap::new("failed to write user agent family")),
                        }
                        match memory.write(major, maj.as_bytes()) {
                            Ok(bytes) => memory.write_i32(major_written, bytes as i32),
                            _ => return Err(Trap::new("failed to write user agent major version")),
                        }
                        match memory.write(minor, min.as_bytes()) {
                            Ok(bytes) => memory.write_i32(minor_written, bytes as i32),
                            _ => return Err(Trap::new("failed to write user agent min version")),
                        }
                        match memory.write(patch, pat.as_bytes()) {
                            Ok(bytes) => memory.write_i32(patch_written, bytes as i32),
                            _ => return Err(Trap::new("failed to write user agent patch version")),
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
