#![cfg_attr(not(test), no_std)]

use core::cmp::min;
use core::fmt::Debug;
use embedded_hal_async::{
    delay::DelayNs,
    i2c::{Error as I2cError, ErrorType as I2cErrorType, I2c},
};
use embedded_storage_async::nor_flash::{
    ErrorType as StorageErrorType, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};

// TODO: These are only valid for AT24CM01. Implement the others
const PAGE_SIZE: usize = 256;
const ADDRESS_BYTES: usize = 2;

// Adds up to 6ms after which the at24x should definitely be ready
const POLL_MAX_RETRIES: usize = 60;
const POLL_DELAY_US: u32 = 100;

/// Custom error type for the various errors that can be thrown by AT24Cx
/// Can be converted into a NorFlashError.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error<E: Debug> {
    I2cError(E),
    NotAligned,
    OutOfBounds,
    WriteEnableFail,
    ReadbackFail,
    WriteAckTimeout,
}

impl<E: Debug> NorFlashError for Error<E> {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Error::NotAligned => NorFlashErrorKind::NotAligned,
            Error::OutOfBounds => NorFlashErrorKind::OutOfBounds,
            _ => NorFlashErrorKind::Other,
        }
    }
}

impl<E: I2cError> From<E> for Error<E> {
    fn from(error: E) -> Self {
        Error::I2cError(error)
    }
}

pub struct Address(pub u8, pub u8);

impl From<Address> for u8 {
    fn from(a: Address) -> Self {
        0x50 | (a.1 << 2) | (a.0 << 1)
    }
}

pub struct At24Cx<I2C, D> {
    address_bits: usize,
    base_address: u8,
    delay: D,
    i2c: I2C,
}

impl<I2C, E: Debug, D: DelayNs> At24Cx<I2C, D>
where
    I2C: I2c<Error = E>,
{
    pub fn new(i2c: I2C, address: Address, address_bits: usize, delay: D) -> Self {
        Self {
            address_bits,
            base_address: address.into(),
            delay,
            i2c,
        }
    }

    fn get_device_address(&self, memory_address: u32) -> Result<u8, Error<E>> {
        if memory_address >= (1 << self.address_bits) {
            return Err(Error::OutOfBounds);
        }
        let p0 = if memory_address & 1 << 16 == 0 { 0 } else { 1 };
        Ok(self.base_address | p0)
    }

    async fn poll_ack(&mut self, offset: u32) -> Result<(), Error<E>> {
        let device_addr = self.get_device_address(offset)?;
        let mut empty = [0];
        for _ in 0..POLL_MAX_RETRIES {
            match self.i2c.read(device_addr, &mut empty).await {
                Ok(_) => return Ok(()), // ACK received, write cycle complete
                Err(_) => {
                    // NACK received, wait a bit and try again
                    self.delay.delay_us(POLL_DELAY_US).await;
                }
            }
        }
        // Timeout waiting for ACK
        Err(Error::WriteAckTimeout)
    }

    async fn page_write(&mut self, address: u32, data: &[u8]) -> Result<(), Error<E>> {
        if data.is_empty() {
            return Ok(());
        }

        // check this before to ensure that data.len() fits into u32
        // ($page_size always fits as its maximum value is 256).
        if data.len() > PAGE_SIZE {
            // This would actually be supported by the EEPROM but
            // the data in the page would be overwritten
            return Err(Error::OutOfBounds);
        }

        let page_boundary = address | (PAGE_SIZE as u32 - 1);
        if address + data.len() as u32 > page_boundary + 1 {
            // This would actually be supported by the EEPROM but
            // the data in the page would be overwritten
            return Err(Error::OutOfBounds);
        }
        //
        let device_addr = self.get_device_address(address)?;
        let mut payload: [u8; ADDRESS_BYTES + PAGE_SIZE] = [0; ADDRESS_BYTES + PAGE_SIZE];
        payload[0] = (address >> 8) as u8;
        payload[1] = address as u8;
        payload[ADDRESS_BYTES..ADDRESS_BYTES + data.len()].copy_from_slice(data);
        self.i2c
            .write(device_addr, &payload[..ADDRESS_BYTES + data.len()])
            .await
            .map_err(Error::I2cError)
    }
}

impl<I2C, E: Debug, D: DelayNs> StorageErrorType for At24Cx<I2C, D>
where
    I2C: I2cErrorType<Error = E>,
{
    type Error = Error<E>;
}

impl<I2C, E: Debug, D: DelayNs> ReadNorFlash for At24Cx<I2C, D>
where
    I2C: I2c<Error = E>,
{
    const READ_SIZE: usize = 1;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        match check_read(self, offset, bytes.len()) {
            Err(NorFlashErrorKind::NotAligned) => return Err(Error::NotAligned),
            Err(_) => return Err(Error::OutOfBounds),
            Ok(_) => {}
        }
        let device_address = self.get_device_address(offset)?;
        let memaddr = [(offset >> 8) as u8, offset as u8];
        self.i2c
            .write_read(device_address, &memaddr[..2], bytes)
            .await
            .map_err(Error::I2cError)
    }

    fn capacity(&self) -> usize {
        1 << self.address_bits
    }
}

impl<I2C, E: Debug, D: DelayNs> NorFlash for At24Cx<I2C, D>
where
    I2C: I2c<Error = E>,
    E: Into<Error<E>>,
{
    const WRITE_SIZE: usize = 1;

    const ERASE_SIZE: usize = PAGE_SIZE;

    async fn erase(&mut self, _from: u32, _to: u32) -> Result<(), Self::Error> {
        // No explicit erase needed
        Ok(())
    }

    async fn write(&mut self, mut offset: u32, mut bytes: &[u8]) -> Result<(), Self::Error> {
        match check_write(self, offset, bytes.len()) {
            Err(NorFlashErrorKind::NotAligned) => return Err(Error::NotAligned),
            Err(_) => return Err(Error::OutOfBounds),
            Ok(_) => {}
        }
        while !bytes.is_empty() {
            let this_page_offset = offset as usize % PAGE_SIZE;
            let this_page_remaining = PAGE_SIZE - this_page_offset;
            let chunk_size = min(bytes.len(), this_page_remaining);
            self.page_write(offset, &bytes[..chunk_size]).await?;
            offset += chunk_size as u32;
            bytes = &bytes[chunk_size..];
            self.poll_ack(offset).await?;
        }
        Ok(())
    }
}

// Copied from https://github.com/rust-embedded-community/embedded-storage/blob/master/src/nor_flash.rs
// TODO: It's not in the async version yet
fn check_slice<T: ReadNorFlash>(
    flash: &T,
    align: usize,
    offset: u32,
    length: usize,
) -> Result<(), NorFlashErrorKind> {
    let offset = offset as usize;
    if length > flash.capacity() || offset > flash.capacity() - length {
        return Err(NorFlashErrorKind::OutOfBounds);
    }
    if offset % align != 0 || length % align != 0 {
        return Err(NorFlashErrorKind::NotAligned);
    }
    Ok(())
}

// Copied from https://github.com/rust-embedded-community/embedded-storage/blob/master/src/nor_flash.rs
// TODO: It's not in the async version yet
/// Return whether a read operation is within bounds.
fn check_read<T: ReadNorFlash>(
    flash: &T,
    offset: u32,
    length: usize,
) -> Result<(), NorFlashErrorKind> {
    check_slice(flash, T::READ_SIZE, offset, length)
}

// Copied from https://github.com/rust-embedded-community/embedded-storage/blob/master/src/nor_flash.rs
// TODO: It's not in the async version yet
/// Return whether a write operation is aligned and within bounds.
fn check_write<T: NorFlash>(
    flash: &T,
    offset: u32,
    length: usize,
) -> Result<(), NorFlashErrorKind> {
    check_slice(flash, T::WRITE_SIZE, offset, length)
}

#[cfg(test)]
mod tests {
    use super::*;
}
