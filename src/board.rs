//! Pimoroni Presto board definition.
//!
//! Initialise the HAL, set up the board-specific peripherals, and hand back a struct containing
//! them (in the style of `microbit-bsp`).

use embassy_rp::Peri;
use embassy_rp::bind_interrupts;
use embassy_rp::clocks::{ClockConfig, PeriClkSrc, PllConfig, SysClkConfig, SysClkSrc, clk_sys_freq};
pub use embassy_rp::config::Config;
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::i2c::{self, Async, InterruptHandler as I2cInterruptHandler};
pub use embassy_rp::interrupt::Priority;
use embassy_rp::peripherals::{
    DMA_CH0, DMA_CH5, DMA_CH6, DMA_CH7, I2C0, I2C1, PIN_33, PIN_34, PIN_35, PIN_36, PIN_37, PIN_38,
    PIN_23, PIN_24, PIN_25, PIN_29, PIN_39, PIN_40, PIN_41, PIN_43, PIN_46, PIO0, PIO2, FLASH, SPI0,
    UART0, UART1,
};
use embassy_rp::pio::{InterruptHandler, Pio};
use embassy_rp::pio_programs::ws2812::{Grb, PioWs2812, PioWs2812Program};
use embassy_rp::psram::Psram;
use embassy_rp::pwm::{self, SetDutyCycle};
use embassy_rp::spi::Spi;
use fixed::{FixedU16, types::extra::U4};

use crate::lcd::{St7701, St7701Pins};
use crate::touch::Ft6236;

/// Data pin for the on-board SK6812 LED strip.
pub type LedDataPin = Peri<'static, PIN_33>;
/// Piezo speaker connection.
pub type SpeakerPin = Peri<'static, PIN_43>;
/// User button pin.
pub type ButtonPin = Peri<'static, PIN_46>;
/// Helper type for the on-board user button.
pub type Button = Input<'static>;
/// Number of LEDs in the on-board SK6812 strip.
pub const BOARD_LED_COUNT: usize = 7;
/// Helper type for the on-board SK6812/WS2812-compatible LED driver.
pub type BoardWs2812<'d> = PioWs2812<'d, PIO0, 0, BOARD_LED_COUNT, Grb>;
/// Helper type for the on-board LCD driver.
pub type BoardLcd<'d> = St7701<'d>;
/// Helper type for the on-board touch controller.
pub type BoardTouch = Ft6236<i2c::I2c<'static, I2C1, Async>, Input<'static>, Output<'static>>;

/// Backlight PWM frequency. The AP3031 boost converter wants a dimming signal in the tens of kHz;
/// low frequencies make it shut down and soft-start every period, which flickers.
const BACKLIGHT_PWM_HZ: u32 = 25_000;

/// LCD backlight (AP3031 boost converter enable on GPIO45, PWM dimmed).
pub struct Backlight {
    pwm: pwm::Pwm<'static>,
    percent: u8,
}

impl Backlight {
    fn new(pwm: pwm::Pwm<'static>) -> Self {
        let mut backlight = Self { pwm, percent: 0 };
        backlight.set_percent(0);
        backlight
    }

    /// Set the brightness as a percentage (0 = off, 100 = full). Perceptually linear.
    pub fn set_percent(&mut self, percent: u8) {
        let percent = percent.min(100);
        self.percent = percent;
        let max = self.pwm.max_duty_cycle() as u32;
        // Square law: perceived brightness is roughly the square root of luminance.
        let duty = (max * percent as u32 * percent as u32) / (100 * 100);
        let _ = self.pwm.set_duty_cycle(duty as u16);
    }

    /// The last brightness set with [`set_percent`](Self::set_percent).
    pub fn percent(&self) -> u8 {
        self.percent
    }
}

bind_interrupts!(
    /// Interrupt bindings for everything the board uses, plus the spare DMA channels (5–7) and
    /// PIO2. Every DMA channel shares `DMA_IRQ_0` and a binding defines the interrupt handler
    /// symbol, so applications must use this struct rather than binding these themselves, e.g.
    /// `dma::Channel::new(board.dma.0, presto_bsp::Irqs)`.
    pub struct Irqs {
        PIO0_IRQ_0 => InterruptHandler<PIO0>;
        PIO1_IRQ_0 => InterruptHandler<embassy_rp::peripherals::PIO1>;
        PIO2_IRQ_0 => InterruptHandler<PIO2>;
        I2C0_IRQ => I2cInterruptHandler<I2C0>;
        I2C1_IRQ => I2cInterruptHandler<I2C1>;
        DMA_IRQ_0 => embassy_rp::dma::InterruptHandler<DMA_CH0>,
            embassy_rp::dma::InterruptHandler<DMA_CH5>,
            embassy_rp::dma::InterruptHandler<DMA_CH6>,
            embassy_rp::dma::InterruptHandler<DMA_CH7>;
    }
);

/// SD card pinout.
pub struct SdCardPins {
    /// SDIO data line 0.
    pub data0: Peri<'static, PIN_36>,
    /// SDIO data line 1.
    pub data1: Peri<'static, PIN_37>,
    /// SDIO data line 2.
    pub data2: Peri<'static, PIN_38>,
    /// SDIO data line 3.
    pub data3: Peri<'static, PIN_39>,
    /// SDIO clock.
    pub clk: Peri<'static, PIN_34>,
    /// SDIO command.
    pub cmd: Peri<'static, PIN_35>,
}

/// Qwiic / STEMMA QT connector (I2C0).
pub struct QwiicPins {
    /// I2C0 peripheral.
    pub i2c0: Peri<'static, I2C0>,
    /// SDA on GPIO40.
    pub sda: Peri<'static, PIN_40>,
    /// SCL on GPIO41.
    pub scl: Peri<'static, PIN_41>,
}

impl QwiicPins {
    /// Create an async I2C bus on the Qwiic connector.
    pub fn into_async(self, config: i2c::Config) -> i2c::I2c<'static, I2C0, Async> {
        i2c::I2c::new_async(self.i2c0, self.scl, self.sda, Irqs, config)
    }
}

/// Peripherals for the on-board SK6812 strip.
pub struct LedPins {
    /// PIO block 0.
    pub pio0: Peri<'static, PIO0>,
    /// DMA channel 0.
    pub dma: Peri<'static, DMA_CH0>,
    /// GPIO33 -> SK6812 data.
    pub data: LedDataPin,
}

impl LedPins {
    /// Configure PIO0 and DMA channel 0 for driving the on-board SK6812 strip.
    pub fn into_ws2812(self) -> BoardWs2812<'static> {
        let Pio { mut common, sm0, .. } = Pio::new(self.pio0, Irqs);
        let program = PioWs2812Program::new(&mut common);
        PioWs2812::new(&mut common, sm0, self.dma, Irqs, self.data, &program)
    }
}

/// RM2 (CYW43439) Wi-Fi/Bluetooth module, driven over PIO SPI (e.g. with `cyw43-pio`).
pub struct WifiPins {
    /// GPIO23, WL_ON: module power. Low keeps the radio completely off.
    pub pwr: Peri<'static, PIN_23>,
    /// GPIO24, WL_D: SPI data in/out (also the host-wake interrupt).
    pub dio: Peri<'static, PIN_24>,
    /// GPIO25, WL_CS: SPI chip select.
    pub cs: Peri<'static, PIN_25>,
    /// GPIO29, WL_CLK: SPI clock.
    pub clk: Peri<'static, PIN_29>,
    /// PIO2, free for the Wi-Fi SPI (PIO0 drives the LEDs, PIO1 the display).
    pub pio: Peri<'static, PIO2>,
}

/// Represents all peripherals/pins exposed for the Pimoroni Presto.
pub struct Presto {
    /// On-board LED strip (not yet configured).
    pub leds: LedPins,
    /// GPIO43 -> Piezo speaker.
    pub speaker: SpeakerPin,
    /// LCD backlight. Off until the application turns it on.
    pub backlight: Backlight,
    /// GPIO46 -> User button (pulled up, active low).
    pub button: Button,
    /// SD card connections on GPIO34-39.
    pub sd: SdCardPins,
    /// Qwiic / STEMMA QT I2C connector on GPIO40/41.
    pub qwiic: QwiicPins,
    /// SPI0 peripheral (for add-ons).
    pub spi0: Peri<'static, SPI0>,
    /// UART0 peripheral.
    pub uart0: Peri<'static, UART0>,
    /// UART1 peripheral.
    pub uart1: Peri<'static, UART1>,
    /// Spare DMA channels.
    pub dma: (Peri<'static, DMA_CH5>, Peri<'static, DMA_CH6>, Peri<'static, DMA_CH7>),
    /// RM2 Wi-Fi module. Its power pin is held low (radio off) until a driver takes it.
    pub wifi: WifiPins,
    /// QSPI flash (16 MiB), e.g. for storing settings with `embassy_rp::flash`.
    pub flash: Peri<'static, FLASH>,
    /// FT6236 touch controller on its dedicated I2C1 bus.
    pub touch: BoardTouch,
    /// LCD driver. The panel is initialised but scan-out has not started; call
    /// [`St7701::start`] with a [`FrameBuffer`](crate::lcd::FrameBuffer).
    /// Uses PIO1 and DMA channels 1-4; with a PSRAM frame buffer also core 1 and DMA channel 8.
    pub lcd: BoardLcd<'static>,
    /// 8 MiB PSRAM, mapped at [`Psram::base_address`], or `None` if it failed to initialise.
    pub psram: Option<Psram<'static>>,
}

/// Write and read back a few words through the uncached PSRAM alias. The chip ID check alone
/// does not catch a bad clock or timing configuration.
fn psram_works(psram: &Psram<'_>) -> bool {
    // Uncached alias of the CS1 window (cached at 0x1100_0000).
    let uncached = (psram.base_address() as usize - 0x1100_0000 + 0x1500_0000) as *mut u32;
    let patterns = [0x1234_5678u32, 0xa5a5_5a5a, 0xdead_beef, 0x0f0f_f0f0];
    for (i, &pattern) in patterns.iter().enumerate() {
        // Spread the probes across the chip (0, 2, 4 and 6 MiB).
        let word = unsafe { uncached.add(i * psram.size() / 4 / 4) };
        unsafe {
            core::ptr::write_volatile(word, pattern);
            if core::ptr::read_volatile(word) != pattern {
                return false;
            }
        }
    }
    true
}

impl Default for Presto {
    fn default() -> Self {
        Self::new(Self::default_config())
    }
}

impl Presto {
    /// Default HAL configuration for the Presto board (200 MHz sysclk, PLL VCO 1.2 GHz).
    pub fn default_config() -> Config {
        let mut clocks = ClockConfig::crystal(12_000_000);
        if let Some(xosc) = &mut clocks.xosc {
            xosc.sys_pll = Some(PllConfig {
                refdiv: 1,
                fbdiv: 100,
                post_div1: 6,
                post_div2: 1,
            });
        }
        clocks.sys_clk = SysClkConfig {
            src: SysClkSrc::PllSys,
            div_int: 1,
            div_frac: 0,
        };
        clocks.peri_clk_src = Some(PeriClkSrc::Sys);
        Config::new(clocks)
    }

    /// Create a new board instance using the provided HAL configuration.
    ///
    /// Initialises the LCD panel (blocking, about 0.5 s) with the backlight off.
    #[must_use]
    pub fn new(config: Config) -> Self {
        let p = embassy_rp::init(config);

        // Backlight first, so the panel's power-on garbage is never visible.
        let mut pwm_config = pwm::Config::default();
        let top = (clk_sys_freq() / BACKLIGHT_PWM_HZ).min(u16::MAX as u32 + 1) - 1;
        pwm_config.top = top as u16;
        pwm_config.divider = FixedU16::<U4>::from_num(1);
        let backlight = Backlight::new(pwm::Pwm::new_output_b(p.PWM_SLICE10, p.PIN_45, pwm_config));

        // `clock_hz` is the *system* clock; embassy derives the PSRAM clock divider from it and
        // `max_mem_freq` (133 MHz): divisor 2, so the PSRAM runs at 100 MHz.
        let mut psram_config = embassy_rp::psram::Config::aps6404l();
        psram_config.clock_hz = clk_sys_freq();
        let psram = Psram::new(embassy_rp::qmi_cs1::QmiCs1::new(p.QMI_CS1, p.PIN_47), psram_config)
            .ok()
            .filter(psram_works);
        #[cfg(feature = "defmt")]
        defmt::debug!("PSRAM {}", if psram.is_some() { "configured" } else { "not found" });

        let mut lcd_spi_config = embassy_rp::spi::Config::default();
        lcd_spi_config.frequency = 8_000_000;
        let lcd_spi = Spi::new_blocking_txonly(p.SPI1, p.PIN_26, p.PIN_27, lcd_spi_config);
        // The ST7701 command interface uses 9-bit frames (bit 8 = data/command).
        embassy_rp::pac::SPI1.cr1().modify(|w| w.set_sse(false));
        embassy_rp::pac::SPI1.cr0().modify(|w| w.set_dss(0b1000));
        embassy_rp::pac::SPI1.cr1().modify(|w| w.set_sse(true));

        let Pio { mut common, irq0, sm0, sm1, .. } = Pio::new(p.PIO1, Irqs);
        let pins = St7701Pins {
            data: [
                common.make_pio_pin(p.PIN_1),
                common.make_pio_pin(p.PIN_2),
                common.make_pio_pin(p.PIN_3),
                common.make_pio_pin(p.PIN_4),
                common.make_pio_pin(p.PIN_5),
                common.make_pio_pin(p.PIN_6),
                common.make_pio_pin(p.PIN_7),
                common.make_pio_pin(p.PIN_8),
                common.make_pio_pin(p.PIN_9),
                common.make_pio_pin(p.PIN_10),
                common.make_pio_pin(p.PIN_11),
                common.make_pio_pin(p.PIN_12),
                common.make_pio_pin(p.PIN_13),
                common.make_pio_pin(p.PIN_14),
                common.make_pio_pin(p.PIN_15),
                common.make_pio_pin(p.PIN_16),
            ],
            hsync: common.make_pio_pin(p.PIN_19),
            vsync: common.make_pio_pin(p.PIN_20),
            dotclk: common.make_pio_pin(p.PIN_22),
            de: Output::new(p.PIN_21, Level::High),
            unused_lsbs: [Output::new(p.PIN_17, Level::Low), Output::new(p.PIN_18, Level::Low)],
            reset: Output::new(p.PIN_44, Level::High),
            spi_cs: Output::new(p.PIN_28, Level::High),
            spi: lcd_spi,
        };
        let dma = (p.DMA_CH1, p.DMA_CH2, p.DMA_CH3, p.DMA_CH4);
        let mut lcd = St7701::new(&mut common, sm0, sm1, irq0, dma, pins, p.CORE1, p.DMA_CH8);
        lcd.init();

        // Touch controller: hold it out of reset, native 480x480 coordinates.
        let touch_reset = Output::new(p.PIN_42, Level::High);
        let touch_i2c = i2c::I2c::new_async(p.I2C1, p.PIN_31, p.PIN_30, Irqs, Default::default());
        let touch = Ft6236::new(touch_i2c, Input::new(p.PIN_32, Pull::Up), touch_reset);

        Self {
            leds: LedPins {
                pio0: p.PIO0,
                dma: p.DMA_CH0,
                data: p.PIN_33,
            },
            speaker: p.PIN_43,
            backlight,
            button: Input::new(p.PIN_46, Pull::Up),
            sd: SdCardPins {
                data0: p.PIN_36,
                data1: p.PIN_37,
                data2: p.PIN_38,
                data3: p.PIN_39,
                clk: p.PIN_34,
                cmd: p.PIN_35,
            },
            qwiic: QwiicPins {
                i2c0: p.I2C0,
                sda: p.PIN_40,
                scl: p.PIN_41,
            },
            spi0: p.SPI0,
            uart0: p.UART0,
            uart1: p.UART1,
            dma: (p.DMA_CH5, p.DMA_CH6, p.DMA_CH7),
            wifi: WifiPins {
                pwr: p.PIN_23,
                dio: p.PIN_24,
                cs: p.PIN_25,
                clk: p.PIN_29,
                pio: p.PIO2,
            },
            flash: p.FLASH,
            touch,
            lcd,
            psram,
        }
    }
}
