#![no_std]
#![no_main]

use core::cell::RefCell;

use defmt::*;
use display_interface_spi::SPIInterface;
use embassy_embedded_hal::shared_bus::blocking::spi::SpiDeviceWithConfig;
use embassy_executor::Spawner;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::spi;
use embassy_rp::spi::Spi;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use embassy_time::{Delay, Timer};

use embedded_graphics::image::{Image};
use embedded_graphics::mono_font::ascii::FONT_10X20;
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyleBuilder, Rectangle};
use embedded_graphics::text::Text;
use mipidsi::models::ST7789;
use mipidsi::options::{Orientation, Rotation};
use mipidsi::Builder;
use mipidsi::options::ColorInversion;

use tinyqoi::Qoi;

use {defmt_rtt as _, panic_probe as _};


const DISPLAY_FREQ: u32 = 64_000_000;

const DISPLAY_WIDTH: i32 = 320;
const DISPLAY_HEIGHT: i32 = 240;


#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    info!("Start Config");

    let rst = p.PIN_15;
    let bl = p.PIN_13;
    let miso = p.PIN_12;
    let mosi: embassy_rp::peripherals::PIN_11 = p.PIN_11;
    let clk = p.PIN_10;
    let display_cs = p.PIN_9;
    let dcx = p.PIN_8;

    // create SPI
    let mut display_config = spi::Config::default();
    display_config.frequency = DISPLAY_FREQ;
    display_config.phase = spi::Phase::CaptureOnSecondTransition;
    display_config.polarity = spi::Polarity::IdleHigh;

    let spi = Spi::new_blocking(p.SPI1, clk, mosi, miso, display_config.clone()); // touch_config.clone());
    let spi_bus: Mutex<NoopRawMutex, _> = Mutex::new(RefCell::new(spi));

    let display_spi = SpiDeviceWithConfig::new(&spi_bus, Output::new(display_cs, Level::High), display_config);


    // dcx: 0 = command, 1 = data
    let dcx = Output::new(dcx, Level::Low);
    let rst = Output::new(rst, Level::Low);

    // LCD backlight -- initially off
    let mut lcd_bl = Output::new(bl, Level::Low);

    // display interface abstraction from SPI and DC
    let di = SPIInterface::new(display_spi, dcx);

    // Define the display from the display interface and initialize it
    let mut display = Builder::new(ST7789, di)
        .display_size(240, 320)
        .reset_pin(rst)
        .orientation(Orientation::new().rotate(Rotation::Deg90))
        .invert_colors(ColorInversion::Inverted)
        // .with_framebuffer_size(DISPLAY_WIDTH as u16, DISPLAY_HEIGHT as u16)
        .init(&mut Delay)
        .unwrap();
    display.clear(Rgb565::BLACK).unwrap();

    let img_data = include_bytes!("../img/240_dim_fleur.qoi");
    //info!("img_data.len(): {} ", img_data.len());
    let qoi = Qoi::new(img_data).unwrap();
    let img_size = qoi.size();
    let img_inset_point = Point::new(
      (DISPLAY_WIDTH- img_size.width as i32)/2,
      (DISPLAY_HEIGHT - img_size.height as i32)/2 );
    let img1 = Image::new(&qoi, img_inset_point);

    let mut led = Output::new(p.PIN_25, Level::Low);

    // initialize the background fill rectangle (for wipes)
    let fill_rect = Rectangle::new(Point::new(0, 0), 
    Size::new(DISPLAY_WIDTH.try_into().unwrap(), DISPLAY_HEIGHT.try_into().unwrap()) )
    .into_styled(
        PrimitiveStyleBuilder::new()
            .stroke_width(2)
            .stroke_color(Rgb565::RED)
            .fill_color(Rgb565::CSS_BLUE_VIOLET)
            .build(),
    );


    info!("Config done");

    fill_rect.draw(&mut display.color_converted()).unwrap();

    let style = MonoTextStyle::new(&FONT_10X20, Rgb565::GREEN);
    Text::new(
        "FUBARN",
        Point::new(5, 40),
        style,
    )
    .draw(&mut display)
    .unwrap();

    // Enable LCD backlight
    lcd_bl.set_high();

    // create a three-frame animation sequence of image translations
    let back_pt = Point::new(-10, -5);
    let forth_pt = Point::new(10, 5);
    let img0 = img1.translate(back_pt);
    let img2 = img1.translate(forth_pt);

    loop {
        // Timer::after_millis(250).await;
        led.set_high();
        // lcd_bl.set_high();

        fill_rect.draw(&mut display.color_converted()).unwrap();

        img0.draw(&mut display.color_converted()).unwrap(); 
        // bg_img.translate_mut(forth_pt);
        // let _ = bg_img.draw(&mut display.color_converted()).unwrap(); 

        img1.draw(&mut display.color_converted()).unwrap(); 
        // info!("led off!");
        // lcd_bl.set_low();
        // fill_rect.draw(&mut display.color_converted()).unwrap();
        // bg_img.translate_mut(back_pt);
        img2.draw(&mut display.color_converted()).unwrap(); 

        led.set_low();

        // Timer::after_millis(250).await;
    }
}

