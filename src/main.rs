#![no_std]
#![no_main]


use defmt::*;
use portable_atomic::{AtomicUsize, Ordering};

use embassy_embedded_hal::shared_bus::asynch::spi::SpiDevice;
use embassy_executor::{Spawner};
use embassy_rp:: {
    gpio::{Input, Level, Output, Pull}, spi::{self, Async, Spi}
};

use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_time::{Delay, Timer};

use embedded_graphics::{
    prelude::*,
    image::{Image},
    pixelcolor::{Rgb565},
    primitives::{Circle, Primitive, PrimitiveStyle, Line},
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


const NUM_MODES: usize = 2;
const DISPLAY_FREQ: u32 = 64_000_000;

const DISPLAY_WIDTH: usize = 320;
const DISPLAY_HEIGHT: usize = 240;
const PIXEL_SIZE: usize = 2; // RGB565 = 2 bytes per pixel
const FRAME_SIZE_BYTES: usize = DISPLAY_WIDTH * DISPLAY_HEIGHT * PIXEL_SIZE;
static FRAME_BUFFER: StaticCell<[u8; FRAME_SIZE_BYTES]> = StaticCell::new();

static MODE_SETTING: AtomicUsize = AtomicUsize::new(0);


#[embassy_executor::task]
async fn gpio_task(mut pin: Input<'static>) {
    loop {
        let mut mode_val = MODE_SETTING.load(Ordering::Relaxed);
        pin.wait_for_falling_edge().await;
        mode_val = (mode_val + 1) % NUM_MODES;
        MODE_SETTING.store(mode_val, Ordering::Relaxed);
    }
}


#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    info!("Start Config");

    MODE_SETTING.store(0, Ordering::Relaxed);

    let pin = Input::new(p.PIN_22, Pull::Up);
    unwrap!(spawner.spawn(gpio_task(pin)));
    
    let mut led = Output::new(p.PIN_25, Level::Low);

    // LCD display 0: ST7789 pins
    let bl0 = p.PIN_8; // SPI1 RX
    let rst0 = p.PIN_7; // SPI0 TX
    let dcx0 = p.PIN_6; // SPI0 SCK
    let cs0 = p.PIN_5; // SPI0 CSN
    let miso0 = p.PIN_4; // SPI0 RX -- unused
    let mosi0 = p.PIN_3; // SPI0 TX
    let sck0 = p.PIN_2; // SPI0 SCK


    // LCD display 1: ST7789 pins
    let rst1 = p.PIN_15; // SPI1 TX
    let dcx1 = p.PIN_14; // SPI1 RX
    let bl1 = p.PIN_13; // SPI1 CSN
    let miso1 = p.PIN_12; // SPI1 RX -- unused
    let mosi1 = p.PIN_11; // SPI1 TX
    let sck1 = p.PIN_10; // SPI1 SCK
    let cs1 = p.PIN_9; // SPI1 CSN

    
    let mut display_config = spi::Config::default();
    display_config.frequency = DISPLAY_FREQ;
    display_config.phase = spi::Phase::CaptureOnSecondTransition;
    display_config.polarity = spi::Polarity::IdleHigh;


    // create SPI0
    let spi0: Spi<'_, embassy_rp::peripherals::SPI0, Async> = 
        Spi::new(p.SPI0, sck0, mosi0, miso0, p.DMA_CH0, p.DMA_CH1, display_config.clone());
    static SPI0_BUS: StaticCell<Mutex<NoopRawMutex, Spi<'static, embassy_rp::peripherals::SPI0, Async>>> = StaticCell::new();
    let spi0_bus = SPI0_BUS.init(Mutex::new(spi0));
    let spi0_device = SpiDevice::new(spi0_bus, Output::new(cs0, Level::High));

    // dcx: 0 = command, 1 = data
    let dcx0_out = Output::new(dcx0, Level::Low);
    let rst0_out = Output::new(rst0, Level::Low);

    // LCD backlight -- initially off
    let mut bl0_out = Output::new(bl0, Level::Low);

    // display interface abstraction from SPI and DC
    let spi_int0 = SpiInterface::new(spi0_device, dcx0_out);

    // Define the display from the display interface and initialize it
    let mut display0 = Builder::new(ST7789, spi_int0)
        .reset_pin(rst0_out)
        .display_size(240, 320)
        .orientation(Orientation::new().rotate(Rotation::Deg270).flip_horizontal())
        .invert_colors(ColorInversion::Inverted)
        .init(&mut Delay)
        .await
        .unwrap();


    // create SPI1
    let spi1: Spi<'_, embassy_rp::peripherals::SPI1, Async> = 
        Spi::new(p.SPI1, sck1, mosi1, miso1, p.DMA_CH2, p.DMA_CH3, display_config.clone());

    // Create shared SPI1 bus
    static SPI1_BUS: StaticCell<Mutex<NoopRawMutex, Spi<'static, embassy_rp::peripherals::SPI1, Async>>> = StaticCell::new();
    let spi1_bus = SPI1_BUS.init(Mutex::new(spi1));
    let spi1_device = SpiDevice::new(spi1_bus, Output::new(cs1, Level::High));

    // dcx: 0 = command, 1 = data
    let dcx1_out = Output::new(dcx1, Level::Low);
    let rst1_out = Output::new(rst1, Level::Low);

    // LCD backlight -- initially off
    let mut bl1_out = Output::new(bl1, Level::Low);

    // display interface abstraction from SPI and DC
    let spi_int1 = SpiInterface::new(spi1_device, dcx1_out);

    // Define the display from the display interface and initialize it
    let mut display1 = Builder::new(ST7789, spi_int1)
        .reset_pin(rst1_out)
        .display_size(240, 320)
        .orientation(Orientation::new().rotate(Rotation::Deg90))
        .invert_colors(ColorInversion::Inverted)
        .init(&mut Delay)
        .await
        .unwrap();

    
    // Initialize frame buffer
    let frame_buffer = FRAME_BUFFER.init([0; FRAME_SIZE_BYTES]);

    // let img_data = include_bytes!("../img/240_dim_fleur.qoi");
    let eyeframe0 = include_bytes!("../img/calm-eye.qoi");
    let eyeframe1 = include_bytes!("../img/love-eye.qoi");
    let eyeframe2 = include_bytes!("../img/joy-eye.qoi");
    let transp = include_bytes!("../img/eye-frame.qoi");

    let qoi1 = Qoi::new(eyeframe0).unwrap();
    let qoi2 = Qoi::new(eyeframe1).unwrap();
    let qoi3 = Qoi::new(eyeframe2).unwrap();
    let qoi4 = Qoi::new(transp).unwrap();

    let img_size = qoi1.size();
    let inset_x:i32 = (DISPLAY_WIDTH - img_size.width as usize).try_into().unwrap();
    let inset_y:i32 = (DISPLAY_HEIGHT - img_size.height as usize).try_into().unwrap();
    let img_inset_point = Point::new(inset_x/2, inset_y/2);
    let img_no_inset = Point::new(0,0);
    let img1: Image<'_, Qoi<'_>> = Image::new(&qoi1, img_inset_point);
    let img2: Image<'_, Qoi<'_>> = Image::new(&qoi2, img_inset_point);
    let img3: Image<'_, Qoi<'_>> = Image::new(&qoi3, img_inset_point);
    let eyeframe_img: Image<'_, Qoi<'_>> = Image::new(&qoi4, img_no_inset);


    let img_array = [ img1, img2, img3];
    let mut img_idx = 0;


    info!("Config done");

    // Enable LCD backlight
    bl0_out.set_high();
    bl1_out.set_high();

   

    loop {

        led.set_high();

        let mode_val = MODE_SETTING.load(Ordering::Relaxed);
        if mode_val == 0 {
            Timer::after_millis(500).await;
        }

        {
            // Create a framebuffer for drawing the current frame 
            let mut raw_fb =
                RawFrameBuf::<Rgb565, _>::new(frame_buffer.as_mut_slice(), DISPLAY_WIDTH, DISPLAY_HEIGHT);

            // Clear the framebuffer to black

            if mode_val == 1 {
                raw_fb.clear(Rgb565::CSS_MAGENTA).unwrap();
                eyeframe_img.draw(&mut raw_fb.color_converted()).unwrap(); 
                // overdraw the fancy eye stuff
                draw_inner_eye(&mut raw_fb).unwrap();
            }
            else {
                raw_fb.clear(Rgb565::BLACK).unwrap();
                img_array[img_idx].draw(&mut raw_fb.color_converted()).unwrap(); 
                img_idx = (img_idx + 1) % img_array.len();
            }

        }

        // Send the framebuffer data to the display

        display0
            .show_raw_data(0, 0, 
                    DISPLAY_WIDTH.try_into().unwrap(), DISPLAY_HEIGHT.try_into().unwrap(), 
                    frame_buffer)
                .await
                .unwrap();

        display1
            .show_raw_data(0, 0, 
                DISPLAY_WIDTH.try_into().unwrap(), DISPLAY_HEIGHT.try_into().unwrap(), 
                frame_buffer)
            .await
            .unwrap();

        led.set_low();
    }
}




fn draw_inner_eye<T>(display: &mut T) -> Result<(), T::Error>
where
    T: DrawTarget<Color = Rgb565>,
{   
    // this line appears vertical onscreen
    Line::new(Point::new(160, 0), Point::new(160, 240))
        .into_styled(PrimitiveStyle::with_stroke(Rgb565::GREEN, 4))
        .draw(display)?;

    // this line appears horizontal onscreen
     Line::new(Point::new(0, 120), Point::new(320, 120))
        .into_styled(PrimitiveStyle::with_stroke(Rgb565::BLUE, 4))
        .draw(display)?;

    let pupil_ctr: Point = Point::new(150,160);

    let iris_diam: i32 = 120;
    let iris_radius: i32 = iris_diam / 2;
    let iris_radion: i32 = iris_radius.isqrt();
    let iris_top_left = pupil_ctr - Point::new(iris_radion, iris_radion);

    let pupil_diam = iris_diam / 2;
    let pupil_radius = iris_radius / 2;
    let pupil_radion: i32 = pupil_radius.isqrt();
    let pupil_top_left = pupil_ctr - Point::new(pupil_radion, pupil_radion);

    Circle::new(iris_top_left, iris_diam.try_into().unwrap() )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::RED))
        .draw(display)?;

    Circle::new(pupil_top_left, pupil_diam.try_into().unwrap() )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::BLACK))
        .draw(display)?;


    Ok(())
}


