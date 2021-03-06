use std::fs::File;
use std::path::Path;

pub mod cartridge;
mod cpu;
mod dma;
pub mod error;
mod instructions;
pub mod joypad;
mod memory;
pub mod ppu;
mod registers;
mod rtc;
mod timer;

#[cfg(feature = "debug")]
pub mod debug;

pub use cpu::Cpu;
use cpu::Interrupt;
use cartridge::Cartridge;
pub use error::{Error, Result};
use joypad::JoypadEvent;
use memory::MemoryWrite;
use ppu::FrameBuffer;

#[cfg_attr(feature = "save", derive(serde::Serialize), derive(serde::Deserialize))]
/// Gameboy
pub struct Gameboy {
    cpu: Cpu,

    #[cfg(feature = "debug")]
    #[cfg_attr(feature = "save", serde(skip))]
    debugger: debug::Debugger,
}

impl Gameboy {
    const FRAME_FREQUENCY: f64 = 59.7; // Hz

    /// Frame duration, in ns
    pub const FRAME_DURATION: u64 = ((1f64 / Self::FRAME_FREQUENCY) * 1e9) as u64;

    /// Initialize the emulator with an optional ROM.
    ///
    /// If no ROM is provided, the emulator will boot into the CGB BIOS ROM. You can
    /// use `Self::insert` to load a cartridge later.
    pub fn init<P: AsRef<Path>>(rom_path: P, boot_rom: bool, trace: bool) -> Result<Self> {
        let cartridge = Cartridge::from_file(rom_path, boot_rom)?;
        let cpu = Cpu::from_cartridge(cartridge, trace)?;

        #[cfg(feature = "debug")]
        let gameboy = Self {
            cpu,
            debugger: debug::Debugger::new(),
        };

        #[cfg(not(feature = "debug"))]
        let gameboy = Self {
            cpu,
        };

        Ok(gameboy)
    }

    /// Figure out the number of clock cycles we can execute in a single frame
    #[inline]
    fn cycles_per_frame(speed: bool) -> u32 {
        let cycle_time = Cpu::cycle_time(speed);
        Self::FRAME_DURATION as u32 / cycle_time
    }

    /// Run the Gameboy for a single step.
    ///
    /// Returns a tuple of: (pointer to `FrameBuffer`, cycles consumed)
    pub fn step(&mut self) -> (Option<&FrameBuffer>, u32) {
        let speed = self.cpu.speed;

        #[cfg(feature = "debug")]
        // If the debugger is triggered, step into the REPL.
        if self.debugger.triggered(&self.cpu) {
            self.debugger.repl(&mut self.cpu);
        }

        // Execute a step of the CPU
        //
        // This handles interrupt processing and DMA internally.
        let (cycles_taken, _inst) = self.cpu.step();

        let mut interrupts = Vec::new();

        // Update the memory bus
        //
        // Internally, this executes a step for each of:
        //
        // 1. PPU
        // 2. Timer
        // 3. Serial
        // 4. RTC (if present)
        self.cpu.memory.step(cycles_taken, speed, &mut interrupts);

        // Trigger any pending interrupts
        for interrupt in interrupts {
            self.cpu.trigger_interrupt(interrupt);
        }

        if self.cpu.stopped {
            // Reset DIV on speed switch
            self.cpu.memory.write(0xFF04u16, 0u8);
            self.cpu.stopped = false;
        }

        (self.cpu.memory.ppu_mut().frame_buffer(), cycles_taken as u32)
    }

    /// Runs the Gameboy for a single frame.
    pub fn frame(&mut self, joypad_events: Option<&[JoypadEvent]>) {
        let mut cycle = 0;
        let speed = self.cpu.speed;
        let num_cycles = Self::cycles_per_frame(speed);

        while cycle < num_cycles {
            let (_, cycles_taken) = self.step();
            cycle += cycles_taken;
        }

        self.update_joypad(joypad_events);
    }

    pub fn update_joypad(&mut self, joypad_events: Option<&[JoypadEvent]>) {
        if let Some(events) = joypad_events {
            for event in events {
                if self.cpu.memory.joypad().handle_event(event) {
                    self.cpu.trigger_interrupt(Interrupt::Joypad);
                }
            }
        }
    }

    /// Insert a new cartridge and reset the emulator
    pub fn insert<P: AsRef<Path>>(&mut self, rom_path: P, boot_rom: bool) -> Result<()> {
        let cartridge = Cartridge::from_file(rom_path, boot_rom)?;
        self.cpu = Cpu::from_cartridge(cartridge, false)?;
        Ok(())
    }

    /// Load a Gameboy from a save state file on disk.
    #[cfg(feature = "save")]
    pub fn load<P: AsRef<Path>, Q: AsRef<Path>>(rom_path: P, save_path: Q) -> Result<Self> {
        let file = File::open(save_path)?;
        let mut gameboy: Self = bincode::deserialize_from(&file)?;

        // Load ROM and any other cartridge-related info
        gameboy.cpu.memory.controller().load(rom_path)?;

        Ok(gameboy)
    }

    /// Dump the current state of this Gameboy to a file on disk.
    #[cfg(feature = "save")]
    pub fn dump<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let mut file = File::create(path)?;
        bincode::serialize_into(&mut file, self)?;
        Ok(())
    }

    /// Reset the emulator
    pub fn reset(&mut self) {
        // Reset the CPU
        self.cpu.reset();
    }

    pub fn cpu(&mut self) -> &mut Cpu {
        &mut self.cpu
    }

    pub fn speed(&self) -> bool {
        self.cpu.speed
    }

    /// Returns a String containing the serial output of this Gameboy _so far_.
    ///
    /// In other words, this output is cumulative and contains every character
    /// logged to serial since the start of Gameboy.
    pub fn serial_output(&self) -> String {
        self.cpu.memory.io().serial_buffer().into_iter().collect()
    }
}
