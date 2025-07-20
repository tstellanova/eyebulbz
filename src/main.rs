#![no_std]
#![no_main]


use defmt::*;
use embassy_embedded_hal::shared_bus::asynch::spi::SpiDevice;
use embassy_executor::Spawner;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::spi::{self, Async};
use embassy_rp::spi::Spi;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_time::{Delay};

use embedded_graphics::image::{Image};
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;

use static_cell::StaticCell;

use lcd_async::{
    interface::SpiInterface,
    models::ST7789,
    options::{ColorInversion, Orientation, Rotation},
    raw_framebuf::RawFrameBuf,
    Builder,
};

use tinyqoi::Qoi;

use {defmt_rtt as _, panic_probe as _};


const DISPLAY_FREQ: u32 = 64_000_000;

const DISPLAY_WIDTH: usize = 320;
const DISPLAY_HEIGHT: usize = 240;
const PIXEL_SIZE: usize = 2; // RGB565 = 2 bytes per pixel
const FRAME_SIZE_BYTES: usize = DISPLAY_WIDTH * DISPLAY_HEIGHT  * PIXEL_SIZE;
static FRAME_BUFFER: StaticCell<[u8; FRAME_SIZE_BYTES]> = StaticCell::new();


#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    info!("Start Config");

    let rst = p.PIN_15;
    let bl = p.PIN_13;
    let miso = p.PIN_12;
    let mosi   = p.PIN_11;
    let clk = p.PIN_10;
    let display_cs = p.PIN_9;
    let dcx = p.PIN_8;

    // create SPI
    let mut display_config = spi::Config::default();
    display_config.frequency = DISPLAY_FREQ;
    display_config.phase = spi::Phase::CaptureOnSecondTransition;
    display_config.polarity = spi::Polarity::IdleHigh;

    let spi = Spi::new(p.SPI1, clk, mosi, miso, p.DMA_CH0, p.DMA_CH1, display_config.clone());

    // Create shared SPI bus
    static SPI_BUS: StaticCell<Mutex<NoopRawMutex, Spi<'static, embassy_rp::peripherals::SPI1, Async>>> = StaticCell::new();
    let spi_bus = SPI_BUS.init(Mutex::new(spi));
    let spi_device = SpiDevice::new(spi_bus, Output::new(display_cs, Level::High));

    // dcx: 0 = command, 1 = data
    let dcx = Output::new(dcx, Level::Low);
    let rst = Output::new(rst, Level::Low);

    // LCD backlight -- initially off
    let mut lcd_bl = Output::new(bl, Level::Low);

    // display interface abstraction from SPI and DC
    let di = SpiInterface::new(spi_device, dcx);

    // Define the display from the display interface and initialize it
    let mut display = Builder::new(ST7789, di)
        .reset_pin(rst)
        .display_size(240, 320)
        .orientation(Orientation::new().rotate(Rotation::Deg90))
        .invert_colors(ColorInversion::Inverted)
        .init(&mut Delay)
        .await
        .unwrap();

    // Initialize frame buffer
    let frame_buffer = FRAME_BUFFER.init([0; FRAME_SIZE_BYTES]);

    let img_data = include_bytes!("../img/240_dim_fleur.qoi");

    let qoi = Qoi::new(img_data).unwrap();
    let img_size = qoi.size();
    let inset_x:i32 = (DISPLAY_WIDTH - img_size.width as usize).try_into().unwrap() ;
    let inset_y:i32 = (DISPLAY_HEIGHT - img_size.height as usize).try_into().unwrap() ;
    
    let img_inset_point = Point::new(inset_x/2, inset_y/2);
    let img1 = Image::new(&qoi, img_inset_point);

    // create a three-frame animation sequence of image translations
    let back_pt = Point::new(-10, 0);
    let forth_pt = Point::new(10, 0);
    let img0 = img1.translate(back_pt);
    let img2 = img1.translate(forth_pt);
    let img_array = [img0, img1, img2];
    let mut img_idx = 0;

    let mut led = Output::new(p.PIN_25, Level::Low);

    info!("Config done");

    // Enable LCD backlight
    lcd_bl.set_high();


    loop {
        led.set_high();

        // Create a framebuffer for drawing
        let mut raw_fb =
            RawFrameBuf::<Rgb565, _>::new(frame_buffer.as_mut_slice(), DISPLAY_WIDTH, DISPLAY_HEIGHT);

        // Clear the framebuffer to black
        raw_fb.clear(Rgb565::BLACK).unwrap();
        
        img_array[img_idx].draw(&mut raw_fb.color_converted()).unwrap(); 
        img_idx = (img_idx + 1) % 3;

        lcd_bl.set_low();
        // Send the framebuffer data to the display
        display
            .show_raw_data(0, 0, 
                DISPLAY_WIDTH.try_into().unwrap(), DISPLAY_HEIGHT.try_into().unwrap(), 
                frame_buffer)
            .await
            .unwrap();
        lcd_bl.set_high();

        led.set_low();
    }
}

