//! Pimoroni Presto board definition.
//!
//! This follows the same "all-in-one" style as the original micro:bit BSP in
//! this repository: initialise the HAL, set up the board-specific pins, and
//! hand back a struct containing the relevant peripherals.

use embassy_rp::bind_interrupts;
pub use embassy_rp::config::Config;
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::i2c::{self, Async, InterruptHandler as I2cInterruptHandler};
use embassy_rp::pio::{InterruptHandler, Pio};
use embassy_rp::pio_programs::ws2812::{Grb, PioWs2812, PioWs2812Program};
pub use embassy_rp::interrupt::Priority;
use embassy_hal_internal::Peri;
use embassy_rp::peripherals::{
    DMA_CH0, I2C0, I2C1, PIO0, PIO1, PIN_33, PIN_34, PIN_35, PIN_36, PIN_37, PIN_38, PIN_39,
    PIN_40, PIN_41, PIN_43, PIN_45, PIN_46, SPI0, UART0, UART1,
};
use embassy_rp::psram::Psram;
use embassy_rp::spi::Spi;
use crate::{
    lcd::St7701,
    speaker,
    touch::Ft6236,
};

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
/// Number of LEDs in the on-board SK6812 strip.
pub const BOARD_LED_COUNT: usize = 7;
/// Helper type for the on-board SK6812/WS2812-compatible LED driver.
pub type BoardWs2812<'d> = PioWs2812<'d, PIO0, 0, BOARD_LED_COUNT, Grb>;
/// Helper type for the on-board LCD driver.
pub type BoardLcd<'d> = St7701<'d, PIO1, 0>;

bind_interrupts!(struct Ws2812Irqs {
    PIO0_IRQ_0 => InterruptHandler<PIO0>;
});
bind_interrupts!(struct LcdIrqs {
    PIO1_IRQ_0 => InterruptHandler<PIO1>;
});
bind_interrupts!(struct TouchI2cIrqs {
    I2C1_IRQ => I2cInterruptHandler<I2C1>;
});

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

/// Qwiic I2C connector.
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
    /// DMA channel 0, useful with PIO/LEDs.
    pub dma_ch0: Peri<'static, DMA_CH0>,
    /// I2C0 peripheral (can be paired with the exposed connector if desired).
    pub i2c0: Peri<'static, I2C0>,
    /// SPI0 peripheral (handy for displays/add-ons).
    pub spi0: Peri<'static, SPI0>,
    /// UART0 peripheral.
    pub uart0: Peri<'static, UART0>,
    /// UART1 peripheral.
    pub uart1: Peri<'static, UART1>,
    /// FT6236 touch controller I2C pins on GPIO30/31.
    pub touch: Ft6236<i2c::I2c<'static, I2C1, Async>, Input<'static>>,
    /// On-board LCD driver.
    pub lcd: BoardLcd<'static>,
    // PSRAM used for display buffers and slint heap.
    pub psram: Psram<'static>
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

        // configure psram
        let psram_config = embassy_rp::psram::Config::aps6404l();
        let psram = Psram::new(embassy_rp::qmi_cs1::QmiCs1::new(p.QMI_CS1, p.PIN_47), psram_config).unwrap();

        // configure lcd using pio
        let Pio { mut common, sm0: lcd_sm, .. } = Pio::new(p.PIO1, LcdIrqs);
        let lcd_data_pins = [
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
        ];
        let lcd_de = common.make_pio_pin(p.PIN_21);
        let lcd_dotclk = common.make_pio_pin(p.PIN_22);
        let mut _hsync = Output::new(p.PIN_19, Level::High);
        let mut _vsync = Output::new(p.PIN_20, Level::High);
        let mut _reset = Output::new(p.PIN_44, Level::High);
        let mut lcd_spi_config = embassy_rp::spi::Config::default();
        lcd_spi_config.frequency = 8_000_000;
        let lcd_spi = Spi::new_blocking_txonly(p.SPI1, p.PIN_26, p.PIN_27, lcd_spi_config);
        { // force driver to use 9bits
            embassy_rp::pac::SPI1.cr1().write(|w| w.set_sse(false)); 
            embassy_rp::pac::SPI1.cr0().modify(|w| w.set_dss(0b1000));
            embassy_rp::pac::SPI1.cr1().write(|w| w.set_sse(true));
        }
        let lcd_spi_cs = Output::new(p.PIN_28, Level::High);
        let mut lcd = St7701::new(
            &mut common,
            lcd_sm,
            p.DMA_CH1,
            lcd_data_pins,
            lcd_dotclk,
            Some(lcd_de),
            lcd_spi_cs,
            lcd_spi
        );
        lcd.init().unwrap();

        // configure touchscreen
        let touch_i2c = i2c::I2c::new_async(p.I2C1, p.PIN_31, p.PIN_30, TouchI2cIrqs, Default::default());
        let touch_int = Input::new(p.PIN_32, Pull::Up);
        let touch = Ft6236::new(touch_i2c, touch_int, false);
        
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
            touch: touch,
            pio0: p.PIO0,
            dma_ch0: p.DMA_CH0,
            i2c0: p.I2C0,
            spi0: p.SPI0,
            uart0: p.UART0,
            uart1: p.UART1,
            lcd,
            psram,
        }
    }

    /// Configure PIO0 and DMA0 for driving the on-board SK6812 strip.
    /// Consumes PIO0, DMA channel 0, and the LED data pin from the board.
    #[must_use]
    pub fn into_board_ws2812(self) -> BoardWs2812<'static> {
        let Pio { mut common, sm0, .. } = Pio::new(self.pio0, Ws2812Irqs);
        let program = PioWs2812Program::new(&mut common);
        PioWs2812::new(&mut common, sm0, self.dma_ch0, self.led_data, &program)
    }
}
