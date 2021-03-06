use std::io::{self, Read, Write};
use crate::{DispatchError, types::*};

/// A message over the wire
/// Each message refers to an action to carry out on an Object with given arguments defined by the interface that the object implements
/// Messages are decoded and encoded by `#[interface("protocol")]` attributes and should not be used otherwise
#[derive(Debug)]
pub struct Message {
    /// The object instance that the message refers to
    pub object: u32,
    /// The event or request to carry out
    pub opcode: u16,
    /// The untyped arguments to pass to the callee
    pub args: Vec<u32>
}
impl Message {
    /// Decode the next message directly off the wire
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        let mut buffer: [u8; 4] = 0u32.to_ne_bytes();
        reader.read_exact(&mut buffer)?;
        let object = u32::from_ne_bytes(buffer);
        reader.read_exact(&mut buffer)?;
        let p = u32::from_ne_bytes(buffer);
        let mut message_size = (p >> 16) as u16;
        let opcode = p as u16;

        if message_size & 0b11 != 0 || message_size < 8 {
            return Err(io::ErrorKind::InvalidData.into())
        }
        // TODO: A vec with a small stack buffer will see a large speed increase
        let mut args = vec![];
        message_size -= 8;
        while message_size > 0 {
            reader.read_exact(&mut buffer)?;
            args.push(u32::from_ne_bytes(buffer));
            message_size -= 4;
        }

        Ok(Self {
            object,
            opcode,
            args
        })
    }
    /// Send the message along the wire for a given interface version
    pub fn send<W: Write>(self, writer: &mut W) -> io::Result<()> {
        let args_size = self.args.len() * std::mem::size_of::<u32>();
        let message_size = 8 + args_size;
        writer.write_all(&self.object.to_ne_bytes())?;
        let info = (message_size << 16) as u32 | self.opcode as u32;
        writer.write_all(&info.to_ne_bytes())?;
        let args: &[u8] = unsafe { std::slice::from_raw_parts(self.args.as_ptr() as *const u8, args_size) };
        writer.write_all(args)?;
        Ok(())
    }
    /// Get an adapter over the arguments
    pub fn args<'a>(&'a self) -> Args<'a> {
        Args {
            args: &self.args
        }
    }
    /// Push a u32 to the list of arguments
    pub fn push_u32(&mut self, int: u32) {
        self.args.push(int)
    }
    /// Push a i32 to the list of arguments
    pub fn push_i32(&mut self, int: i32) {
        self.args.push(int as _)
    }
    /// Push a Fixed to the list of arguments
    pub fn push_fixed(&mut self, fixed: Fixed) {
        self.args.push(fixed.0 as u32)
    }
    /// Push a string to the list of arguments, appending a null-terminator
    /// Use `push_bytes()` if you are pushing a string that is already null-terminated
    pub fn push_str<Bytes: AsRef<[u8]>>(&mut self, str: Bytes) {
        let chunks = str.as_ref().chunks_exact(std::mem::size_of::<u32>());
        let r = chunks.remainder();
        // As we add a character and div rounds down, we always add an extra u32 to the length 
        self.args.push(str.as_ref().len() as u32 + 1);
        self.args.extend(chunks.map(|b| u32::from_ne_bytes([b[0], b[1], b[2], b[3]])));
        self.args.push(
            match r.len() {
                0 => 0,
                1 => u32::from_ne_bytes([r[0], b'\0', 0, 0]),
                2 => u32::from_ne_bytes([r[0], r[1], b'\0', 0]),
                3 => u32::from_ne_bytes([r[0], r[1], r[2], b'\0']),
                _ => unreachable!()
            }
        )
    }
    /// Push an array of bytes to the list of arguments
    pub fn push_bytes(&mut self, bytes: &[u8]) {
        let chunks = bytes.as_ref().chunks_exact(std::mem::size_of::<u32>());
        let r = chunks.remainder();
        self.args.push(bytes.as_ref().len() as u32 | 0b11);
        self.args.extend(chunks.map(|b| u32::from_ne_bytes([b[0], b[1], b[2], b[3]])));
        match r.len() {
            0 => (),
            1 => self.args.push(u32::from_ne_bytes([r[0], 0, 0, 0])),
            2 => self.args.push(u32::from_ne_bytes([r[0], r[1], 0, 0])),
            3 => self.args.push(u32::from_ne_bytes([r[0], r[1], r[2], 0])),
            _ => unreachable!()
        }
    }
}

/// An adapter over a &[u32] stream for parsing arguments
/// Each access consumes u32's from the stream
pub struct Args<'a> {
    args: &'a [u32]
}
impl<'a> Args<'a> {
    /// Interpret the next argument as an unsigned integer
    pub fn next_u32(&mut self) -> Option<u32> {
        self.args.first().map(|&i| {
            self.args = &self.args[1..];
            i
        })
    }
    /// Interpret the next argument as a signed integer
    pub fn next_i32(&mut self) -> Option<i32> {
        self.args.first().map(|&i| {
            self.args = &self.args[1..];
            i as i32
        })
    }
    /// Interpret the next argument as a Fixed-point decimal
    pub fn next_fixed(&mut self) -> Option<Fixed> {
        self.next_i32().map(|i| Fixed(i))
    }
    /// Interpret the next argument as a byte string
    /// TODO: look into Cow and possibly &str if Wayland strings can be losslessly converted to UTF-8
    pub fn next_str(&mut self) -> Option<&'a [u8]> {
        let mut len = self.next_u32()? as usize;
        // Round up to the next aligned index
        if len & 0b11 != 0 {
            len = (len & !0b11) + 4;
        }
        if len > self.args.len() * std::mem::size_of::<u32>() {
            None
        } else {
            // Transmute to a &[u8], careful to update the length to be in the correct units and to keep the same lifetime
            let str: &'a [u8] = unsafe { std::slice::from_raw_parts(self.args.as_ptr() as *const u8, self.args.len() * std::mem::size_of::<u32>() / std::mem::size_of::<u8>()) };
            self.args = &self.args[len / std::mem::size_of::<u32>()..];
            // TODO: Should we trust the length? Are nulls in a &str potentially hazardous?
            let null_index = str[..len].iter().take_while(|&&b| b != 0).count(); // Too lenient
            Some(&str[..null_index])
        }

    }
    /// Interpret the next argument as a byte slice
    /// Similar to `next_str()` but can contain null bytes
    pub fn next_array(&mut self) -> Option<&'a [u8]> {
        let len = self.next_u32()? as usize;
        // Round up to the next aligned index
        let aligned_len = if len & 0b11 != 0 {
            (len & !0b11) + 4
        } else {
            len
        };
        if self.args.len() * std::mem::size_of::<u32>() < aligned_len {
            None
        } else {
            // Transmute to a &[u8], careful to update the length to be in the correct units and to keep the same lifetime
            let array: &'a [u8] = unsafe { std::slice::from_raw_parts(self.args.as_ptr() as *const u8, len) };
            self.args = &self.args[aligned_len / std::mem::size_of::<u32>()..];
            Some(array)
        }

    }
    /// Interpret the next argument as a new_id of which we do not know the type of
    pub fn next_new_id(&mut self) -> Result<NewId, DispatchError> {
        let interface = std::str::from_utf8(self.next_str().ok_or(DispatchError::ExpectedArgument("new_id interface"))?)
            .map_err(|e| DispatchError::Utf8Error(e, "Interface name for a generic new_id"))?;
        Ok(NewId {
            interface,
            version: self.next_u32().ok_or(DispatchError::ExpectedArgument("new_id version"))?,
            id: self.next_u32().ok_or(DispatchError::ExpectedArgument("new_id id"))?
        })
    }
    /// Collect a file descriptor from the ancillary data
    pub fn next_fd(&mut self) -> ! {
        unimplemented!(/* https://github.com/rust-lang/rust/issues/76915 scheduled for Rust 1.50 */)
    }
}