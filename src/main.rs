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


use embedded_graphics::{
    prelude::*,
    image::{Image},
    pixelcolor::Rgb565,
    primitives::{Circle, Primitive, PrimitiveStyle, Triangle, Ellipse},
};

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
const FRAME_SIZE_BYTES: usize = DISPLAY_WIDTH * DISPLAY_HEIGHT * PIXEL_SIZE;
static FRAME_BUFFER: StaticCell<[u8; FRAME_SIZE_BYTES]> = StaticCell::new();


#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    info!("Start Config");

    // LCD display 0: ST7789 pins
    let rst0 = p.PIN_7; // SPI0 TX
    let bl0 = p.PIN_8; // SPI1 RX
    let miso0 = p.PIN_4; // SPI0 RX
    let mosi0 = p.PIN_3; // SPI0 TX
    let sck0 = p.PIN_2; // SPI0 SCK
    let cs0 = p.PIN_5; // SPI0 CSN
    let dcx0 = p.PIN_6; // SPI0 SCK


    // LCD display 1: ST7789 pins
    let rst1 = p.PIN_15; // SPI1 TX
    let bl1 = p.PIN_13; // SPI1 CSN
    let miso1 = p.PIN_12; // SPI1 RX
    let mosi1   = p.PIN_11; // SPI1 TX
    let sck1 = p.PIN_10; // SPI1 SCK
    let cs1 = p.PIN_9; // SPI1 CSN
    let dcx1 = p.PIN_14; // SPI1 RX

    
    // create SPI
    let mut display_config = spi::Config::default();
    display_config.frequency = DISPLAY_FREQ;
    display_config.phase = spi::Phase::CaptureOnSecondTransition;
    display_config.polarity = spi::Polarity::IdleHigh;

    let spi = Spi::new(p.SPI1, sck1, mosi1, miso1, p.DMA_CH0, p.DMA_CH1, display_config.clone());

    // Create shared SPI bus
    static SPI_BUS: StaticCell<Mutex<NoopRawMutex, Spi<'static, embassy_rp::peripherals::SPI1, Async>>> = StaticCell::new();
    let spi_bus = SPI_BUS.init(Mutex::new(spi));
    let spi_device = SpiDevice::new(spi_bus, Output::new(cs1, Level::High));

    // dcx: 0 = command, 1 = data
    let dcx = Output::new(dcx1, Level::Low);
    let rst = Output::new(rst1, Level::Low);

    // LCD backlight -- initially off
    let mut lcd_bl = Output::new(bl1, Level::Low);

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
    let inset_x:i32 = 0;
    let inset_y:i32 = (DISPLAY_HEIGHT - img_size.height as usize).try_into().unwrap();
    
    let img_inset_point = Point::new(inset_x, inset_y/2);
    let base_img = Image::new(&qoi, img_inset_point);

    // create a three-frame animation sequence of image translations
    let img_array = [
        base_img.translate(Point { x: 5, y: 0 }),
        base_img.translate(Point { x: 15, y: 0 }),
        base_img.translate(Point { x: 25, y: 0 }),
        base_img.translate(Point { x: 35, y: 0 }),
        base_img.translate(Point { x: 45, y: 0 }),
        base_img.translate(Point { x: 55, y: 0 }),
        base_img.translate(Point { x: 65, y: 0 }),
        base_img.translate(Point { x: 75, y: 0 }),
        base_img.translate(Point { x: 85, y: 0 }),
        base_img.translate(Point { x: 95, y: 0 }),
    ];

    let mut img_idx = 0;

    let mut led = Output::new(p.PIN_25, Level::Low);

    info!("Config done");

    // Enable LCD backlight
    lcd_bl.set_high();


    loop {
        led.set_high();

        {
            // Create a framebuffer for drawing the current frame 
            let mut raw_fb =
                RawFrameBuf::<Rgb565, _>::new(frame_buffer.as_mut_slice(), DISPLAY_WIDTH, DISPLAY_HEIGHT);

            // Clear the framebuffer to black
            raw_fb.clear(Rgb565::BLACK).unwrap();
            
            // dump the current image into the buffer
            img_array[img_idx].draw(&mut raw_fb.color_converted()).unwrap(); 
            img_idx = (img_idx + 1) % img_array.len();

            draw_eyeball(&mut raw_fb).unwrap();
        }

        // Send the framebuffer data to the display
        display
            .show_raw_data(0, 0, 
                DISPLAY_WIDTH.try_into().unwrap(), DISPLAY_HEIGHT.try_into().unwrap(), 
                frame_buffer)
            .await
            .unwrap();

        led.set_low();
    }
}



fn draw_eyeball<T>(display: &mut T) -> Result<(), T::Error>
where
    T: DrawTarget<Color = Rgb565>,
{
    let left_pupil_inset = 20;
    let top_eye_inset = 30;
    Ellipse::new(Point::new(0, 10), Size::new(80, 40) )
            .into_styled(PrimitiveStyle::with_fill(Rgb565::WHITE))
            .draw(display)?;
    
    Circle::new(Point::new(left_pupil_inset-(30/2), top_eye_inset-(30/2)), 30)
        .into_styled(PrimitiveStyle::with_fill(Rgb565::RED))
        .draw(display)?;

    Circle::new(Point::new(left_pupil_inset-(20/2), top_eye_inset-(20/2)), 20)
        .into_styled(PrimitiveStyle::with_fill(Rgb565::BLACK))
        .draw(display)?;

    let x_off:i32 = (DISPLAY_WIDTH - 80).try_into().unwrap();
    Ellipse::new(Point::new(x_off, 10), Size::new(80, 40) )
            .into_styled(PrimitiveStyle::with_fill(Rgb565::WHITE))
            .draw(display)?;
    
    Circle::new(Point::new(x_off+20-(30/2), top_eye_inset-(30/2)), 30)
        .into_styled(PrimitiveStyle::with_fill(Rgb565::RED))
        .draw(display)?;

    Circle::new(Point::new(x_off+20-(20/2), top_eye_inset-(20/2)), 20)
        .into_styled(PrimitiveStyle::with_fill(Rgb565::BLACK))
        .draw(display)?;


    Ok(())
}

