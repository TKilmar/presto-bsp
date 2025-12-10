//! Pimoroni Presto board definition.
//!
//! This follows the same "all-in-one" style as the original micro:bit BSP in
//! this repository: initialise the HAL, set up the board-specific pins, and
//! hand back a struct containing the relevant peripherals.

pub use embassy_rp::config::Config;
use embassy_rp::gpio::{Input, Level, Output, Pull};
pub use embassy_rp::interrupt::Priority;
use embassy_hal_internal::Peri;
use embassy_rp::peripherals::{
    DMA_CH0, DMA_CH1, I2C0, I2C1, PIO0, PIO1, PIN_33, PIN_34, PIN_35, PIN_36, PIN_37, PIN_38, PIN_39, PIN_40,
    PIN_41, PIN_43, PIN_45, PIN_46, SPI0, SPI1, UART0, UART1,
};

use crate::{lcd::LcdPlaceholder, speaker};

/// Data pin for the on-board SK6812 LED strip.
pub type LedDataPin = Peri<'static, PIN_33>;
/// Piezo speaker connection.
pub type SpeakerPin = Peri<'static, PIN_43>;
/// LCD backlight enable (AD3031 controller).
pub type BacklightPin = Peri<'static, PIN_45>;
/// User button pin.
pub type ButtonPin = Peri<'static, PIN_46>;

/// Helper type for the backlight output.
pub type Backlight = Output<'static>;
/// Helper type for the on-board user button.
pub type Button = Input<'static>;
/// Helper type for the piezo speaker driver on GPIO43.
pub type PiezoSpeaker<'a> = speaker::SoftSpeaker<'a>;

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

/// Public I2C connector.
pub struct I2cPins {
    /// SDA on GPIO40.
    pub sda: Peri<'static, PIN_40>,
    /// SCL on GPIO41.
    pub scl: Peri<'static, PIN_41>,
}

/// Represents all peripherals/pins exposed for the Pimoroni Presto.
pub struct Presto {
    /// GPIO33 -> SK6812 LED strip data.
    pub led_data: LedDataPin,
    /// GPIO43 -> Piezo speaker.
    pub speaker: SpeakerPin,
    /// GPIO45 -> LCD backlight enable (active high).
    pub backlight: Backlight,
    /// GPIO46 -> User button (pulled up).
    pub button: Button,
    /// SD card connections on GPIO34-39.
    pub sd: SdCardPins,
    /// Exposed I2C bus on GPIO40/41.
    pub i2c: I2cPins,
    /// PIO block 0 for high-speed tasks (LEDs, etc.).
    pub pio0: Peri<'static, PIO0>,
    /// PIO block 1.
    pub pio1: Peri<'static, PIO1>,
    /// DMA channel 0, useful with PIO/LEDs.
    pub dma_ch0: Peri<'static, DMA_CH0>,
    /// DMA channel 1, kept free for applications.
    pub dma_ch1: Peri<'static, DMA_CH1>,
    /// I2C0 peripheral (can be paired with the exposed connector if desired).
    pub i2c0: Peri<'static, I2C0>,
    /// I2C1 peripheral.
    pub i2c1: Peri<'static, I2C1>,
    /// SPI0 peripheral (handy for displays/add-ons).
    pub spi0: Peri<'static, SPI0>,
    /// SPI1 peripheral.
    pub spi1: Peri<'static, SPI1>,
    /// UART0 peripheral.
    pub uart0: Peri<'static, UART0>,
    /// UART1 peripheral.
    pub uart1: Peri<'static, UART1>,
    /// Placeholder for the on-board LCD connection.
    pub lcd: LcdPlaceholder,
}

impl Default for Presto {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl Presto {
    /// Create a new board instance using the provided HAL configuration.
    #[must_use]
    pub fn new(config: embassy_rp::config::Config) -> Self {
        let p = embassy_rp::init(config);

        Self {
            led_data: p.PIN_33,
            speaker: p.PIN_43,
            backlight: Output::new(p.PIN_45, Level::Low),
            button: Input::new(p.PIN_46, Pull::Up),
            sd: SdCardPins {
                data0: p.PIN_36,
                data1: p.PIN_37,
                data2: p.PIN_38,
                data3: p.PIN_39,
                clk: p.PIN_34,
                cmd: p.PIN_35,
            },
            i2c: I2cPins {
                sda: p.PIN_40,
                scl: p.PIN_41,
            },
            pio0: p.PIO0,
            pio1: p.PIO1,
            dma_ch0: p.DMA_CH0,
            dma_ch1: p.DMA_CH1,
            i2c0: p.I2C0,
            i2c1: p.I2C1,
            spi0: p.SPI0,
            spi1: p.SPI1,
            uart0: p.UART0,
            uart1: p.UART1,
            lcd: LcdPlaceholder::new(),
        }
    }
}
