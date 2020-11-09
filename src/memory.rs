use byteorder::{ByteOrder, LittleEndian};
use std::io::{self, Read, Write};
use wasmtime::Memory;

/// Convience api for common write operations
pub trait WriteMem {
    fn write_i32(
        &mut self,
        index: usize,
        value: i32,
    );

    fn write_u32(
        &mut self,
        index: usize,
        value: u32,
    );

    fn write(
        &mut self,
        index: usize,
        bytes: &[u8],
    ) -> io::Result<usize>;
}

impl WriteMem for Memory {
    fn write_i32(
        &mut self,
        index: usize,
        value: i32,
    ) {
        unsafe {
            // one little, two little, three litte Endian...
            LittleEndian::write_i32(&mut self.data_unchecked_mut()[index..], value);
        };
    }

    fn write_u32(
        &mut self,
        index: usize,
        value: u32,
    ) {
        LittleEndian::write_u32(
            unsafe { &mut self.data_unchecked_mut()[index..] },
            value as u32,
        )
    }

    fn write(
        &mut self,
        index: usize,
        bytes: &[u8],
    ) -> std::io::Result<usize> {
        (unsafe { &mut self.data_unchecked_mut()[index..] }).write(bytes)
    }
}

/// Convience api for common read operations
pub trait ReadMem {
    fn read(
        &mut self,
        index: usize,
        amount: usize,
    ) -> std::io::Result<(usize, Vec<u8>)>;
}

impl ReadMem for Memory {
    fn read(
        &mut self,
        index: usize,
        amount: usize,
    ) -> std::io::Result<(usize, Vec<u8>)> {
        let mut buf = Vec::with_capacity(amount);
        let mut slice = unsafe { &self.data_unchecked_mut()[index as usize..] };
        let num = (&mut slice).take(amount as u64).read_to_end(&mut buf)?;
        Ok((num, buf))
    }
}
