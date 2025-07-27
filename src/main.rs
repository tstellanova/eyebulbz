#![no_std]
#![no_main]


use defmt::*;
use portable_atomic::{AtomicUsize, Ordering};

use embassy_embedded_hal::shared_bus::asynch::spi::SpiDevice;
use embassy_executor::{Spawner};
use embassy_rp:: {
    gpio::{Input, Level, Output, Pull}, spi::{self, Async, Spi},
};

use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_time::{Delay};

use embedded_graphics::{
    image::Image, pixelcolor::Rgb565, prelude::*, primitives::{Arc, Circle, Ellipse, Line, Primitive, PrimitiveStyle, Rectangle}
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
type FullFrameBuf = [u8; FRAME_SIZE_BYTES];
static SINGLE_FRAMEBUF: StaticCell<FullFrameBuf> = StaticCell::new();

const IRIS_FRAME_WIDTH: usize = 125;
const IRIS_FRAME_HEIGHT: usize = 125;
const IRIS_FRAME_EXTENT: i32 = IRIS_FRAME_WIDTH as i32;
const IRIS_REGION_SIZE_BYTES: usize = IRIS_FRAME_WIDTH * IRIS_FRAME_HEIGHT * PIXEL_SIZE;
static IRIS_FRAMEBUF: StaticCell<[u8; IRIS_REGION_SIZE_BYTES]> = StaticCell::new();

static MODE_SETTING: AtomicUsize = AtomicUsize::new(0);

type RealDisplayType<T>=lcd_async::Display<SpiInterface<SpiDevice<'static, NoopRawMutex, Spi<'static, T, embassy_rp::spi::Async>, Output<'static>>, Output<'static>>, ST7789, Output<'static>>;


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
    let mut left_display = Builder::new(ST7789, spi_int0)
        .reset_pin(rst0_out)
        .display_size(240, 320)
        .orientation(Orientation::new().rotate(Rotation::Deg270))
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
    let mut right_display = Builder::new(ST7789, spi_int1)
        .reset_pin(rst1_out)
        .display_size(240, 320)
        .orientation(Orientation::new().rotate(Rotation::Deg90))
        .invert_colors(ColorInversion::Inverted)
        .init(&mut Delay)
        .await
        .unwrap();
    
    // Initialize frame buffer
    let single_frame_buf  = SINGLE_FRAMEBUF.init([0; FRAME_SIZE_BYTES]);
    let iris_frame_buf = IRIS_FRAMEBUF.init([0; IRIS_REGION_SIZE_BYTES]);

    let eyeframe_left_qoi = Qoi::new(include_bytes!("../img/eye-frame-left-olive.qoi")).unwrap();
    let eyeframe_left_img: Image<'_, Qoi<'_>> = Image::new(&eyeframe_left_qoi, Point::new(0,0));
    
    let eyeframe_right_qoi = Qoi::new(include_bytes!("../img/eye-frame-right-olive.qoi")).unwrap();
    let eyeframe_right_img: Image<'_, Qoi<'_>> = Image::new(&eyeframe_right_qoi, Point::new(0,0));

    let left_pupil_ctr: Point = Point::new(320-148,159);
    let right_pupil_ctr: Point = Point::new(148,159);
    let iris_diam = 122;
    let pupil_diam = iris_diam / 2;
    let highlight_size = Size::new(26 , 28);
    let iris_radius: i32 = iris_diam / 2;
    let left_iris_tl = Point::new(left_pupil_ctr.x - iris_radius, left_pupil_ctr.y - iris_radius + 25);
    let right_iris_tl = Point::new(right_pupil_ctr.x - iris_radius, right_pupil_ctr.y - iris_radius);
    let left_inner_pupil_ctr = Point::new(IRIS_FRAME_EXTENT/2, IRIS_FRAME_EXTENT/2);//center in inner frame

    let iris_diam_dim: u32 = iris_diam.try_into().unwrap();
    let inset_iris_clip_tl = Point::new(left_iris_tl.x, left_iris_tl.y + 25);
    let left_iris_clip_rect = Rectangle::new(Point::new(0,25), 
        Size::new(iris_diam_dim+2, iris_diam_dim+2 - 25));

    info!("Config done");

    // draw initial background
    redraw_one_background(
        &mut left_display, 
        single_frame_buf,
        &eyeframe_left_img).await;
    redraw_one_background(
        &mut right_display, 
        single_frame_buf,
        &eyeframe_right_img).await;

  

    // Enable LCD backlight
    bl0_out.set_high();
    bl1_out.set_high();

    let mut loop_count: usize = 0;
    let iris_colors = [Rgb565::CSS_FIRE_BRICK, Rgb565::RED, Rgb565::CSS_DARK_MAGENTA, Rgb565::CSS_MEDIUM_VIOLET_RED, Rgb565::CSS_PALE_VIOLET_RED];
    // Main drawing loop, runs forever
    loop {
        led.set_high();
        let mode_val = MODE_SETTING.load(Ordering::Relaxed);

         let iris_color =
            if mode_val == 0 { Rgb565::CSS_MAGENTA }
            else { iris_colors[loop_count % iris_colors.len()] };
    
        //draw left inner eyeball
        {
            let mut inner_fb = 
                        RawFrameBuf::<Rgb565, _>::new(iris_frame_buf.as_mut_slice(), 
                        IRIS_FRAME_WIDTH, IRIS_FRAME_HEIGHT);
            inner_fb.clear(Rgb565::CSS_DARK_OLIVE_GREEN).unwrap();

            // overdraw the fancy eye stuff
            draw_one_inner_eye(&mut inner_fb, true, 
                &left_inner_pupil_ctr, 
                iris_diam, pupil_diam, &highlight_size, iris_color).unwrap();
                        
            const TOTAL_TAIL_BYTES:usize = IRIS_FRAME_WIDTH * (IRIS_FRAME_HEIGHT - 25) * PIXEL_SIZE;

            left_display
                .show_raw_data(left_iris_tl.x.try_into().unwrap(), 
                left_iris_tl.y.try_into().unwrap(), 
                    IRIS_FRAME_WIDTH.try_into().unwrap(), 
                    IRIS_FRAME_HEIGHT.try_into().unwrap(), 
                    &iris_frame_buf[iris_frame_buf.len() - TOTAL_TAIL_BYTES..])
                .await
                .unwrap();
        }

        // draw right frame
        {
            let mut raw_fb =
                RawFrameBuf::<Rgb565, _>::new(single_frame_buf.as_mut_slice(), DISPLAY_WIDTH, DISPLAY_HEIGHT);
            // raw_fb.clear(Rgb565::BLACK).unwrap();
            // eyeframe_right_img.draw(&mut raw_fb.color_converted()).unwrap(); 
            // overdraw the fancy eye stuff
            draw_one_inner_eye(&mut raw_fb, false, &right_pupil_ctr, iris_diam, pupil_diam, &highlight_size,iris_color).unwrap();

            right_display
            .show_raw_data(0, 0, 
                DISPLAY_WIDTH.try_into().unwrap(), DISPLAY_HEIGHT.try_into().unwrap(), 
                single_frame_buf)
            .await
            .unwrap();
        }

        loop_count += 1;
        led.set_low();
    }
}

async fn redraw_one_background<T>(display: &mut RealDisplayType<T>, 
    base_frame_buf: &mut FullFrameBuf, 
    bg_img: &Image<'_, Qoi<'_>>) 
    where T: embassy_rp::spi::Instance

{       
    let mut raw_fb =
        RawFrameBuf::<Rgb565, _>::new(base_frame_buf.as_mut_slice(), DISPLAY_WIDTH, DISPLAY_HEIGHT);
    // raw_fb.clear(Rgb565::BLACK).unwrap();
    bg_img.draw(&mut raw_fb.color_converted()).unwrap(); 
    let _ = display
        .show_raw_data(0, 0, 
            DISPLAY_WIDTH.try_into().unwrap(), DISPLAY_HEIGHT.try_into().unwrap(), 
            base_frame_buf)
            .await;
    
}

fn draw_one_inner_eye<T>(
    display: &mut T, 
    is_left: bool, 
    pupil_ctr: &Point, 
    iris_diam: i32, 
    pupil_diam: i32, 
    highlight_size: &Size,
    iris_color: Rgb565) -> Result<(), T::Error>
where
    T: DrawTarget<Color = Rgb565>,
{   
    // // this line appears vertical onscreen
    // Line::new(Point::new(160, 0), Point::new(160, 240))
    //     .into_styled(PrimitiveStyle::with_stroke(Rgb565::GREEN, 1))
    //     .draw(display)?;

    // // this line appears horizontal onscreen
    //  Line::new(Point::new(0, 120), Point::new(320, 120))
    //     .into_styled(PrimitiveStyle::with_stroke(Rgb565::BLUE, 1))
    //     .draw(display)?;

    let iris_diam_dim: u32 = iris_diam.try_into().unwrap();
    let iris_radius: i32 = iris_diam / 2;
    let iris_tl = Point::new(pupil_ctr.x - iris_radius, pupil_ctr.y - iris_radius);
    let iris_bg_tl = Point::new(iris_tl.x - 2, iris_tl.y -2 );
    let inset_iris_clip_tl = Point::new(iris_tl.x, iris_tl.y + 25);
    let clip_rect = Rectangle::new(inset_iris_clip_tl, 
        Size::new(iris_diam_dim, iris_diam_dim - 25));

    let pupil_radius = pupil_diam / 2;
    let pupil_top_left = Point::new(pupil_ctr.x - pupil_radius, pupil_ctr.y - pupil_radius);

    let highlight_tl = Point::new( pupil_top_left.x - 10, pupil_top_left .y - 5);
    let small_highlight_tl = Point::new(highlight_tl.x + pupil_diam, highlight_tl.y + pupil_diam);

    // draw a slightly larger black backdrop to iris
    Circle::new(iris_bg_tl, iris_diam_dim + 4 )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::BLACK))
        .draw( display)?;

    if !is_left {
        //redraw eyelash
        let eyelash_ctr = Point::new(pupil_ctr.x, 240);
         Arc::with_center(eyelash_ctr, 280, -45.0.deg(), -90.0.deg())
                .into_styled(PrimitiveStyle::with_stroke(Rgb565::CYAN, 6 ))
                .draw(display)?;
    }
    let mut clippy = display.clipped(&clip_rect);

    Circle::new(iris_tl, iris_diam_dim )
        .into_styled(PrimitiveStyle::with_fill(iris_color))
        .draw(&mut clippy)?;

    Circle::new(pupil_top_left, pupil_diam.try_into().unwrap() )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::BLACK))
        .draw(&mut clippy)?;

    Ellipse::new(highlight_tl, *highlight_size)
        .into_styled(PrimitiveStyle::with_fill(Rgb565::WHITE))
        .draw(&mut clippy)?;

    Circle::new(small_highlight_tl, 14) 
        .into_styled(PrimitiveStyle::with_fill(Rgb565::WHITE))
        .draw(&mut clippy)?;


    Ok(())
}
