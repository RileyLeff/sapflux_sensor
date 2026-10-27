//! W25Q256JV on SPI2 (PB13 SCK, PB14 MISO, PB15 MOSI, PB12 CS#; see
//! `elec/main.ato`). Implements [`sap_flux_core::flash::Nor`] with the
//! 4-byte-address opcodes; operation tracking and timeouts live in
//! [`sap_flux_core::flash::Flash`].

use embassy_stm32::gpio::Output;
use embassy_stm32::mode::Blocking;
use embassy_stm32::spi::mode::Master;
use embassy_stm32::spi::Spi;
use sap_flux_core::flash::Nor;

const CMD_WRITE_ENABLE: u8 = 0x06;
const CMD_READ_STATUS1: u8 = 0x05;
const CMD_JEDEC_ID: u8 = 0x9F;
const CMD_POWER_DOWN: u8 = 0xB9;
const CMD_RELEASE_POWER_DOWN: u8 = 0xAB;
const CMD_READ_4B: u8 = 0x13;
const CMD_PAGE_PROGRAM_4B: u8 = 0x12;
const CMD_SECTOR_ERASE_4B: u8 = 0x21;
const STATUS1_BUSY: u8 = 0x01;

pub struct W25q<'d> {
    spi: Spi<'d, Blocking, Master>,
    cs: Output<'d>,
}

impl<'d> W25q<'d> {
    pub fn new(spi: Spi<'d, Blocking, Master>, mut cs: Output<'d>) -> Self {
        cs.set_high();
        Self { spi, cs }
    }

    /// Leave deep power-down if a previous firmware entered it. The chip
    /// needs tRES1 (3 us) before the next command.
    pub fn release_power_down(&mut self) -> bool {
        self.transaction(&[CMD_RELEASE_POWER_DOWN], &mut [])
    }

    /// Enter deep power-down (1 uA typ instead of 10 uA standby). Only
    /// [`Self::release_power_down`] is accepted until then; the chip needs
    /// tDP (3 us) to get there. Call only while no program or erase runs.
    pub fn power_down(&mut self) -> bool {
        self.transaction(&[CMD_POWER_DOWN], &mut [])
    }

    pub fn jedec_id(&mut self) -> Option<[u8; 3]> {
        let mut id = [0u8; 3];
        self.transaction(&[CMD_JEDEC_ID], &mut id).then_some(id)
    }

    fn transaction(&mut self, command: &[u8], read: &mut [u8]) -> bool {
        self.cs.set_low();
        let ok = self.spi.blocking_write(command).is_ok()
            && (read.is_empty() || self.spi.blocking_read(read).is_ok());
        self.cs.set_high();
        ok
    }

    fn write_enable(&mut self) -> bool {
        self.transaction(&[CMD_WRITE_ENABLE], &mut [])
    }
}

fn addressed(op: u8, addr: u32) -> [u8; 5] {
    let a = addr.to_be_bytes();
    [op, a[0], a[1], a[2], a[3]]
}

impl Nor for W25q<'_> {
    fn read(&mut self, addr: u32, buf: &mut [u8]) -> bool {
        self.transaction(&addressed(CMD_READ_4B, addr), buf)
    }

    fn program(&mut self, addr: u32, data: &[u8]) -> bool {
        if !self.write_enable() {
            return false;
        }
        self.cs.set_low();
        let ok = self
            .spi
            .blocking_write(&addressed(CMD_PAGE_PROGRAM_4B, addr))
            .is_ok()
            && self.spi.blocking_write(data).is_ok();
        self.cs.set_high();
        ok
    }

    fn erase_sector(&mut self, addr: u32) -> bool {
        self.write_enable() && self.transaction(&addressed(CMD_SECTOR_ERASE_4B, addr), &mut [])
    }

    fn busy(&mut self) -> Option<bool> {
        let mut status = [0u8; 1];
        self.transaction(&[CMD_READ_STATUS1], &mut status)
            .then_some(status[0] & STATUS1_BUSY != 0)
    }
}
