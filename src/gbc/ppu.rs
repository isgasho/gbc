//! Gameboy PPU and LCD handling
//!
//! # Overview
//!
//! ## Background
//!
//! The Gameboy screen buffer holds 256x256 pixels, or 32x32 *tiles*.
//! However, the LCD only displays 160x144 pixels at a time.
//!
//! `SCROLLX` and `SCROLLY` registers contain the location of the background at the
//! upper left of the screen (i.e., it is scrollable). Note that the background
//! wraps around the screen edges.
//!
//! The *Background Tile Map* contains 32 rows of 32 bytes each. Each byte
//! maps to a tile number. Tiles are stored in the *Tile Data Table* in VRAM
//! in one of these two regions:
//!
//! * 0x8000-0x8FFF (tile number = 0 to 255)
//! * 0x8800-0x97FF (tile number = -128 to 127, 0th tile at 0x9000)
//!
//! The region is set/modified using the LCDC register.
//!
//! *BG Display Data*, or the actual content of the background (256 x 256 pixels),
//! is stored at either:
//!
//! * 0x9800-0x9BFF
//! * 0x9C00-0x9FFF
//!
//! The region is set using bit 3 of the LCDC register.
//!
//! The aforementioned scroll registers determine which area of the BG is displayed
//! on the 160x144 LCD.
//!
//! ## Window
//!
//! WX and WY control where the window is displayed on the LCD. Note that the window
//! does not wrap and is not scrollable.
//!
//! ## LCD
//!
//! Each row of 160 pixels takes 108.7 us to display. If you multiply that by 144 rows,
//! the total display time is ~15.66 ms.
//!
//! Once the frame is displayed, the VBLANK period lasts 10 lines, which maps to ~1.09 ms.
//! This is when VRAM data can be accessed.
//!
//! The combination of these two periods nets us ~60 fps.
use crate::memory::{MemoryRead, MemoryWrite};

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(u8)]
enum StatMode {
    Hblank = 0,
    Vblank,
    OamScan,
    OamRead,
}

pub enum Vram {
    Unbanked {
        /// Static bank, 8K
        ///
        /// Non-CGB mode
        data: Box<[u8; Self::BANK_SIZE]>,
    },
    Banked {
        /// Two static banks, 8K each
        ///
        /// CGB mode
        data: Box<[u8; Self::BANK_SIZE * 2]>,
        active_bank: u8,
    },
}

impl Vram {
    const BANK_SIZE: usize = 8 * 1024;
    pub const BASE_ADDR: u16 = 0x8000;
    pub const LAST_ADDR: u16 = 0x9FFF;
    pub const BANK_SELECT_ADDR: u16 = 0xFF4F;

    pub fn new(cgb: bool) -> Self {
        if cgb {
            Self::Banked {
                data: Box::new([0u8; Self::BANK_SIZE * 2]),
                active_bank: 0,
            }
        } else {
            Self::Unbanked {
                data: Box::new([0u8; Self::BANK_SIZE]),
            }
        }
    }

    /// Update the active VRAM bank
    pub fn update_bank(&mut self, bank: u8) {
        match self {
            Self::Banked {
                data: _,
                active_bank,
            } => {
                assert!(bank < 2);
                *active_bank = bank;
            }
            _ => panic!("Received VRAM bank change request on unbanked VRAM"),
        }
    }
}

impl MemoryRead<u16, u8> for Vram {
    #[inline]
    fn read(&self, addr: u16) -> u8 {
        let addr = (addr - Self::BASE_ADDR) as usize;
        match self {
            Self::Unbanked { data } => {
                // Bank 0 (static)
                data[addr]
            }
            Self::Banked { data, active_bank } => {
                let bank_offset = *active_bank as usize * Self::BANK_SIZE;
                data[bank_offset + addr]
            }
        }
    }
}

impl MemoryWrite<u16, u8> for Vram {
    #[inline]
    fn write(&mut self, addr: u16, value: u8) {
        let addr = (addr - Self::BASE_ADDR) as usize;
        match self {
            Self::Unbanked { data } => {
                // Bank 0 (static)
                data[addr] = value;
            }
            Self::Banked { data, active_bank } => {
                let bank_offset = *active_bank as usize * Self::BANK_SIZE;
                data[bank_offset + addr] = value;
            }
        }
    }
}

impl std::fmt::Debug for Vram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unbanked { data } => f
                .debug_struct("Vram::Unbanked")
                .field("vram_size", &data.len())
                .finish(),
            Self::Banked { data, active_bank } => f
                .debug_struct("Vram::Banked")
                .field("vram_size", &data.len())
                .field("active_bank", &active_bank)
                .finish(),
        }
    }
}

#[derive(Debug)]
pub struct Ppu {
    /// Video RAM (0x8000 - 0x9FFF)
    vram: Vram,

    /// OAM (0xFE00-0xFE9F)
    ///
    /// 40 objects can be loaded into this RAM. Each 4-byte object
    /// consists of:
    ///
    /// 1. y-coordinate (8 bits)
    /// 2. x-coordinate (8 bits)
    /// 3. CHR code (8 bits)
    /// 4. BG and OBJ display priority (1 bit)
    /// 5. Vertical flip (1 bit)
    /// 6. Horizontal flip (1 bit)
    /// 6. DMG mode palette (1 bit)
    /// 7. Character blank (1 bit)
    /// 8. Color palette (3 bits)
    oam: [u8; 160],

    /// LCD control register (0xFF40)
    lcdc: u8,

    /// LCD status register (0xFF41)
    stat: u8,

    /// Background position registers (0xFF42, 0xFF43)
    ///
    /// Range: 0x00-0xFF (256 x 256 pixels)
    scy: u8,
    scx: u8,

    /// LCD line registers (0xFF44, 0xFF45)
    ly: u8,
    lyc: u8,

    /// Window position registers (0xFF4A, 0xFF4B)
    ///
    /// Ranges: 0 <= WY <= 143 and 7 <= WX <= 166
    wy: u8,
    wx: u8,

    /// Color data registers (0xFF68-0xFF6B)
    bcps: u8,
    bcpd: u8,
    ocps: u8,
    ocpd: u8,

    /// Interrupt enable flags
    oam_enabled: bool,
    vblank_enabled: bool,
    hblank_enabled: bool,
    ly_enabled: bool,
}

impl Ppu {
    const STAT_ADDR: u16 = 0xFF41;
    const LY_ADDR: u16 = 0xFF44;
    const DOT_DURATION_NS: u32 = 238; // 4.1924 MHz
    const DOTS_PER_LINE: u32 = 456;
    const VBLANK_START_LINE: u8 = 144;

    pub fn new(cgb: bool) -> Self {
        Self {
            vram: Vram::new(cgb),
            oam: [0u8; 160],
            lcdc: 0,
            stat: 0,
            scy: 0,
            scx: 0,
            ly: 0,
            lyc: 0,
            wy: 0,
            wx: 0,
            bcps: 0,
            bcpd: 0,
            ocps: 0,
            ocpd: 0,
            oam_enabled: false,
            vblank_enabled: false,
            hblank_enabled: false,
            ly_enabled: false,
        }
    }

    /// Update the PPU status registers based on current cycle and cycle time.
    ///
    /// Returned tuple contains two interrupt flags: (Vblank, LcdStat)
    ///
    /// This function is called once per frame from the main frame loop.
    pub fn update(&mut self, cycle: u32, cycle_time: u32) -> (bool, bool) {
        // Figure out the current dot and scan line
        let dot = cycle / (cycle_time / Self::DOT_DURATION_NS);
        let line = (dot / Self::DOTS_PER_LINE) as u8;

        // Set LY to current scan line
        self.ly = line;

        // Compute LY conincidence
        let prev_ly_coincidence = (self.stat & 1 << 2) != 0;
        let ly_coincidence = self.ly == self.lyc;

        // Figure out which stat mode we are in based on line and dot
        let prev_mode = self.stat & 0x3;
        let mode = if line < Self::VBLANK_START_LINE {
            let dot = dot % Self::DOTS_PER_LINE;
            match dot {
                0..=79 => StatMode::OamScan,
                80..=329 => StatMode::OamRead,
                _ => StatMode::Hblank,
            }
        } else {
            // VBLANK
            StatMode::Vblank
        };

        // Update STAT register
        let mut stat = mode as u8;
        if ly_coincidence {
            stat |= 1 << 2;
        }

        self.stat = stat;

        // VBLANK interrupt is fired if the mode has changed and the current mode is VBLANK.
        //
        // Note: This is fired once per frame.
        let vblank_interrupt = prev_mode != mode as u8 && mode == StatMode::Vblank;

        // If any of the STAT interrupt conditions are met, fire an interrupt.
        //
        // 1. LY coincidence interrupt is enabled and changed from false to true
        // 2. STAT mode interrupt is enabled and has changed
        let stat_interrupt = (self.ly_enabled && !prev_ly_coincidence && ly_coincidence) || {
            prev_mode != mode as u8 && match mode {
                StatMode::Hblank => self.hblank_enabled,
                StatMode::Vblank => self.vblank_enabled,
                StatMode::OamScan | StatMode::OamRead => self.oam_enabled,
            }
        };

        (vblank_interrupt, stat_interrupt)
    }

    pub fn vram(&self) -> &Vram {
        &self.vram
    }

    pub fn vram_mut(&mut self) -> &mut Vram {
        &mut self.vram
    }
}

impl MemoryRead<u16, u8> for Ppu {
    #[inline]
    fn read(&self, addr: u16) -> u8 {
        match addr {
            Vram::BASE_ADDR..=Vram::LAST_ADDR => self.vram.read(addr),
            0xFE00..=0xFE9F => {
                let idx = (addr as usize) - 0xFE00;
                self.oam[idx]
            }
            0xFF40 => self.lcdc,
            Self::STAT_ADDR => self.stat,
            0xFF42 => self.scy,
            0xFF43 => self.scx,
            Self::LY_ADDR => self.ly,
            0xFF45 => self.lyc,
            0xFF4A => self.wy,
            0xFF4B => self.wx,
            0xFF68 => self.bcps,
            0xFF69 => self.bcpd,
            0xFF6A => self.ocps,
            0xFF6B => self.ocpd,
            _ => panic!("Unexpected read from addr {}", addr),
        }
    }
}

impl MemoryWrite<u16, u8> for Ppu {
    #[inline]
    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            Vram::BASE_ADDR..=Vram::LAST_ADDR => self.vram.write(addr, value),
            0xFE00..=0xFE9F => {
                let idx = (addr as usize) - 0xFE00;
                self.oam[idx] = value;
            }
            0xFF40 => self.lcdc = value,
            Self::STAT_ADDR => {
                self.stat = value;
                self.hblank_enabled = self.stat & (1 << 3) != 0;
                self.vblank_enabled = self.stat & (1 << 4) != 0;
                self.oam_enabled = self.stat & (1 << 5) != 0;
                self.ly_enabled = self.stat & (1 << 6) != 0;
            }
            0xFF42 => self.scy = value,
            0xFF43 => self.scx = value,
            Self::LY_ADDR => {
                if self.ly & (1 << 7) != 0 {
                    // If bit 7 == 1 and it is getting reset, clear out LY entirely
                    if value & (1 << 7) == 0 {
                        self.ly = 0;
                    }
                } else {
                    // If bit 7 == 0, accept all writes
                    self.ly = value;
                }
            }
            0xFF45 => self.lyc = value,
            0xFF4A => self.wy = value,
            0xFF4B => self.wx = value,
            0xFF68 => self.bcps = value,
            0xFF69 => {
                self.bcpd = value;

                if self.bcps & (1 << 7) != 0 {
                    // Auto increment BCPS on write to BCPD (wrapping)
                    let mut bcp_index = self.bcps & 0x3F + 1;
                    if bcp_index > 0x3F {
                        bcp_index = 0x00;
                    }

                    self.bcps = bcp_index;
                }
            }
            0xFF6A => self.ocps = value,
            0xFF6B => self.ocpd = value,
            _ => panic!("Unexpected write to addr {} value {}", addr, value),
        }
    }
}
