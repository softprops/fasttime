use byteorder::{ByteOrder, LittleEndian};
use std::io::{self, Read, Write};
use wasmtime::Memory;

/// Convience api for common write operations
pub trait WriteMem {
    fn write_i32(
        &mut self,
        index: i32,
        value: i32,
    );

    fn write_u32(
        &mut self,
        index: i32,
        value: u32,
    );

    fn write(
        &mut self,
        index: i32,
        bytes: &[u8],
    ) -> io::Result<usize>;
}

impl WriteMem for Memory {
    fn write_i32(
        &mut self,
        index: i32,
        value: i32,
    ) {
        unsafe {
            // one little, two little, three litte Endian...
            LittleEndian::write_i32(&mut self.data_unchecked_mut()[index as usize..], value);
        };
    }

    fn write_u32(
        &mut self,
        index: i32,
        value: u32,
    ) {
        LittleEndian::write_u32(
            unsafe { &mut self.data_unchecked_mut()[index as usize..] },
            value as u32,
        )
    }

    fn write(
        &mut self,
        index: i32,
        bytes: &[u8],
    ) -> std::io::Result<usize> {
        (unsafe { &mut self.data_unchecked_mut()[index as usize..] }).write(bytes)
    }
}

/// Convience api for common read operations
pub trait ReadMem {
    fn read(
        &mut self,
        index: i32,
        amount: i32,
    ) -> std::io::Result<(usize, Vec<u8>)>;
}

impl ReadMem for Memory {
    fn read(
        &mut self,
        index: i32,
        amount: i32,
    ) -> std::io::Result<(usize, Vec<u8>)> {
        let mut buf = Vec::with_capacity(amount as usize);
        let mut slice = unsafe { &self.data_unchecked_mut()[index as usize..] };
        let num = (&mut slice).take(amount as u64).read_to_end(&mut buf)?;
        Ok((num, buf))
    }
}
