#![no_std]
#![no_main]

use {defmt_rtt as _, panic_probe as _};

use defmt::*;
use embedded_hal::spi::SpiBus;
// use rp235x_hal::rosc::RingOscillator;
use core::default::Default;
use portable_atomic::{AtomicU16, AtomicUsize, Ordering};

use embassy_embedded_hal::shared_bus::asynch::spi::SpiDevice;
use embassy_executor::{Spawner, Executor};
use embassy_rp:: {
    self as hal, block::ImageDef, gpio::{Input, Level, Output, Pull}, peripherals::{self, SPI1}, pwm::{self, Pwm, SetDutyCycle}, spi::{self, Async, Spi}
};

use embassy_sync::{blocking_mutex::raw::{self, NoopRawMutex}, mutex::Mutex};
use embassy_time::{Delay, Instant, Timer};

use embedded_graphics::{
    image::Image, pixelcolor::{raw::RawU16, Rgb565}, prelude::{DrawTargetExt, *}, primitives::{Arc, Circle, Ellipse, Primitive, PrimitiveStyle, PrimitiveStyleBuilder, Rectangle, Styled}
};


use embassy_rp::multicore::Stack;

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

/// Tell the Boot ROM about our application
#[link_section = ".start_block"]
#[used]
pub static IMAGE_DEF: ImageDef = hal::block::ImageDef::secure_exe();

// Program metadata for `picotool info`.
// This isn't needed, but it's recomended to have these minimal entries.
#[link_section = ".bi_entries"]
#[used]
pub static PICOTOOL_ENTRIES: [embassy_rp::binary_info::EntryAddr; 4] = [
    embassy_rp::binary_info::rp_program_name!(c"Kerplonk"),
    embassy_rp::binary_info::rp_program_description!(
        c"Testing drawing with dual displays" 
    ),
    embassy_rp::binary_info::rp_cargo_version!(),
    embassy_rp::binary_info::rp_program_build_attribute!(),
];


const DISPLAY_FREQ: u32 = 64_000_000;

const DISPLAY_WIDTH: u16 =  320; 
const DISPLAY_HEIGHT: u16 = 240; 
const PIXEL_SIZE: u16 = 2; // RGB565 = 2 bytes per pixel
const FRAME_SIZE_BYTES: usize = DISPLAY_WIDTH as usize * DISPLAY_HEIGHT as usize * PIXEL_SIZE as usize;
type FullFrameBuf = [u8; FRAME_SIZE_BYTES];

const IRIS_DIAM: u16 = 122;
const PUPIL_DIAM: u16 = IRIS_DIAM/2;
const HIGHLIGHT_DIAM: i32 = 30;

const INNER_EYE_DIM: u16 = IRIS_DIAM + 4;

const FARPOINT_CENTER: Point = Point::new(160, 240);

const EYELASH_DIAMETER: u32 = 310u32;


const LEFT_PUPIL_CTR: Point = Point::new((DISPLAY_WIDTH-148) as i32,159) ; //- Size::new(0, DISPLAY_HEIGHT as u32 / 2);
const RIGHT_PUPIL_CTR: Point = Point::new(148,159); //  - Size::new(0, DISPLAY_HEIGHT as u32 / 2);

const NUM_A_MODES: usize = 5;

const IRIS_PALETTE_PURPLE: [Rgb565; 8] = [ 
    Rgb565::CSS_INDIGO, 
    Rgb565::CSS_REBECCAPURPLE,  
    Rgb565::CSS_DARK_ORCHID,
    Rgb565::CSS_BLUE_VIOLET,
    Rgb565::CSS_MEDIUM_PURPLE,
    Rgb565::CSS_MEDIUM_ORCHID,
    Rgb565::CSS_VIOLET,
    Rgb565::CSS_PLUM,

];
const IRIS_PALETTE_SPECTRUM: [Rgb565; 6] = [ Rgb565::CSS_BLUE_VIOLET, Rgb565::CSS_DARK_MAGENTA,  Rgb565::CSS_MEDIUM_VIOLET_RED, Rgb565::CSS_PALE_VIOLET_RED, Rgb565::CSS_YELLOW_GREEN, Rgb565::CSS_LIME_GREEN];

#[link_section = ".core1_stack"]
static mut CORE1_STACK: Stack<4096> = Stack::new();
// static mut CORE1_STACK: [u8; 4096] = [0; 4096]; // Example: 4KB stack

// static EXECUTOR0: StaticCell<Executor> = StaticCell::new();
static EXECUTOR1: StaticCell<Executor> = StaticCell::new();


static DISPLAY0_FRAMEBUF: StaticCell<FullFrameBuf> = StaticCell::new();
static DISPLAY1_FRAMEBUF: StaticCell<FullFrameBuf> = StaticCell::new();

static MODE_SETTING: AtomicUsize = AtomicUsize::new(3);
static CUR_BRIGHTNESS: AtomicUsize = AtomicUsize::new(50);
static CUR_IRIS_COLOR: AtomicU16 = AtomicU16::new(0);

type RealDisplayType<T>=lcd_async::Display<SpiInterface<SpiDevice<'static, NoopRawMutex, Spi<'static, T, embassy_rp::spi::Async>, Output<'static>>, Output<'static>>, ST7789, Output<'static>>;


fn render_one_bg_image<T>(
    frame_buf: &mut FullFrameBuf, 
    bg_img: &Image<'_, T>) 
    where T: ImageDrawable,  Rgb565: From<<T as embedded_graphics::image::ImageDrawable>::Color>
{       
    let mut raw_fb =
        RawFrameBuf::<Rgb565, _>::new(frame_buf.as_mut_slice(), DISPLAY_WIDTH as usize, DISPLAY_HEIGHT as usize);
    bg_img.draw(&mut raw_fb.color_converted()).unwrap(); 

    build_styled_arc(FARPOINT_CENTER + Size::new(0,30), EYELASH_DIAMETER+30, 
        -45.0, -90.0, Rgb565::CYAN, 8).draw(&mut raw_fb).unwrap();

    // eyelash outer
    build_styled_arc(FARPOINT_CENTER, EYELASH_DIAMETER, 
        -35.0, -110.0, Rgb565::CSS_INDIGO, 12).draw(&mut raw_fb).unwrap();

}

fn build_styled_arc(center: Point, diam: u32, start_deg: f32, sweep_deg: f32, color: Rgb565, stroke_width: u32) -> Styled<Arc, PrimitiveStyle<Rgb565>> {
    Styled::new(
        Arc::with_center(center, 
            diam, 
            Angle::from_degrees(start_deg), 
        Angle::from_degrees(sweep_deg)),
        PrimitiveStyle::with_stroke(color, stroke_width),
    )
}

fn draw_elliptic_inner_eye<T>(
    display: &mut T, 
    pupil_ctr: &Point, 
    iris_diam: i32, 
    pupil_diam: i32, 
    iris_color: Rgb565,
    _highlight_diam: i32,
    look_correction: f32,
) -> Result<(), T::Error>
where
    T: DrawTarget<Color = Rgb565>,
{   
    let pupil_diam_dim: u32 = pupil_diam.try_into().unwrap();
    let iris_diam_dim: u32 = iris_diam.try_into().unwrap();

    // temporary -- render an idealized inner eye clipping region
    let _ = Rectangle::with_center(*pupil_ctr, Size::new(INNER_EYE_DIM.into(),INNER_EYE_DIM.into()))
    .into_styled(PrimitiveStyle::with_stroke(Rgb565::CSS_YELLOW, 3))
    .draw(display);

    // fill with bg color
    let _ = Circle::with_center(*pupil_ctr, iris_diam_dim + 15) 
        .into_styled(PrimitiveStyle::with_fill(Rgb565::CSS_DARK_OLIVE_GREEN))
        .draw(display);

    let iris_style = PrimitiveStyleBuilder::new()
            .stroke_width(2)
            .stroke_color(Rgb565::BLACK)
            .fill_color(iris_color)
            .build();
    
    let iris_size = Size::new((iris_diam_dim as f32 * look_correction.abs()) as u32, iris_diam_dim);
    let offset_iris_ctr = 
        if look_correction == 1.0 {
            pupil_ctr.clone()
        }
        else if look_correction > 0. {
            *pupil_ctr + Size::new(10, 0)
        }
        else {
            *pupil_ctr - Size::new(10, 0)
        };
    Ellipse::with_center(offset_iris_ctr, iris_size)
        .into_styled(iris_style)
        .draw(display)?;    

    // // shaded iris
    // let shaded_iris_color = Rgb565::new(iris_color.r()/2, iris_color.g()/2, iris_color.b()/2);
    // let iris_shade_start = Angle::from_degrees(-15.0);
    // let iris_shade_sweep = Angle::from_degrees(-180.0 + 15.0) - iris_shade_start;
    // Sector::with_center(*pupil_ctr, iris_diam_dim, iris_shade_start, iris_shade_sweep)
    //     .into_styled(PrimitiveStyle::with_fill(shaded_iris_color))
    //     .draw(display)?;

    // pupil
    let pupil_size = Size::new((pupil_diam_dim as f32 * look_correction.abs()) as u32, pupil_diam_dim);
    let offset_pupil_ctr = 
        if look_correction == 1.0 {
            pupil_ctr.clone()
        }
        else if look_correction > 0. {
            *pupil_ctr + Size::new(20, 0)
        }
        else {
            *pupil_ctr - Size::new(20, 0)
        };
    Ellipse::with_center(offset_pupil_ctr, pupil_size)
        .into_styled(PrimitiveStyle::with_fill(Rgb565::BLACK))
        .draw(display)?; 

    // draw stuff that shades iris
    // eyelash inner (liner)
    build_styled_arc(FARPOINT_CENTER + Size::new(0,30), EYELASH_DIAMETER+30, 
        -45.0, -90.0, Rgb565::CYAN, 8).draw(display)?;

    // eyelash outer
    // build_styled_arc(FARPOINT_CENTER, EYELASH_DIAMETER, 
    //     -35.0, -110.0, Rgb565::CSS_INDIGO, 12).draw(display)?;

    const HIGHLIGHT_X_SHIFT: i32 = 8;
    const HIGHLIGHT_Y_SHIFT: i32 = 4;
    let pupil_half_width = pupil_size.width as i32 / 2;
    let pupil_half_height = pupil_size.height as i32 / 2;

    // two highlights are symmetric about the pupil center
    let highlight_ctr = Point::new(offset_pupil_ctr.x - pupil_half_width + HIGHLIGHT_X_SHIFT,  (offset_pupil_ctr.y - pupil_half_height) + HIGHLIGHT_Y_SHIFT );
    let small_highlight_ctr = Point::new(offset_pupil_ctr.x + pupil_half_width - HIGHLIGHT_X_SHIFT, (offset_pupil_ctr.y + pupil_half_height) - HIGHLIGHT_Y_SHIFT);

    // lens highlight large
    Ellipse::with_center(highlight_ctr, pupil_size/2)
        .into_styled(PrimitiveStyle::with_fill(Rgb565::WHITE))
        .draw(display)?; 

    // lens highlight small 
    Ellipse::with_center(small_highlight_ctr, pupil_size/4)
        .into_styled(PrimitiveStyle::with_fill(Rgb565::WHITE))
        .draw(display)?; 

    Ok(())
}


// fn draw_symmetric_inner_eye<T>(
//     display: &mut T, 
//     pupil_ctr: &Point, 
//     iris_diam: i32, 
//     pupil_diam: i32, 
//     iris_color: Rgb565) -> Result<(), T::Error>
// where
//     T: DrawTarget<Color = Rgb565>,
// {   
//     let pupil_diam_dim: u32 = pupil_diam.try_into().unwrap();
//     let iris_diam_dim: u32 = iris_diam.try_into().unwrap();

//     let iris_style = PrimitiveStyleBuilder::new()
//         .stroke_width(2)
//         .stroke_color(Rgb565::BLACK)
//         .fill_color(iris_color)
//         .build();

//     // // behind iris
//     // Circle::with_center(*pupil_ctr, iris_diam_dim + 4)
//     //     .into_styled(PrimitiveStyle::with_fill(Rgb565::BLACK))
//     //     .draw(display)?;

//     // iris
//     Circle::with_center(*pupil_ctr, iris_diam_dim)
//         .into_styled(iris_style)
//         .draw(display)?;

//     // shaded iris
//     let shaded_iris_color = Rgb565::new(iris_color.r()/2, iris_color.g()/2, iris_color.b()/2);
//     let iris_shade_start = Angle::from_degrees(-15.0);
//     let iris_shade_sweep = Angle::from_degrees(-180.0 + 15.0) - iris_shade_start;
//     Sector::with_center(*pupil_ctr, iris_diam_dim, iris_shade_start, iris_shade_sweep)
//         .into_styled(PrimitiveStyle::with_fill(shaded_iris_color))
//         .draw(display)?;

//     // pupil
//     Circle::with_center(*pupil_ctr, pupil_diam_dim )
//         .into_styled(PrimitiveStyle::with_fill(Rgb565::BLACK))
//         .draw(display)?;

//     // draw stuff that shades iris
//     // eyelash inner (liner)
//     build_styled_arc(FARPOINT_CENTER + Size::new(0,30), EYELASH_DIAMETER+30, 
//         -45.0, -90.0, Rgb565::CYAN, 8).draw(display)?;

//     // eyelash outer
//     build_styled_arc(FARPOINT_CENTER, EYELASH_DIAMETER, 
//         -35.0, -110.0, Rgb565::CSS_INDIGO, 12).draw(display)?;

//     Ok(())
// }

// fn draw_asymmetric_inner_eye<T>(
//     display: &mut T, 
//     _is_left: bool, 
//     eye_ctr: &Point,  
//     pupil_diam: i32, 
//     highlight_diam: i32,
// )
// where
//     T: DrawTarget<Color = Rgb565>,
// {   
//     const HIGHLIGHT_Y_SHIFT: i32 = 8;
//     let pupil_radius = pupil_diam / 2;
//     let highlight_diam_dim: u32 = highlight_diam.try_into().unwrap();

//     // two highlights are symmetric about the pupil center
//     let highlight_ctr = Point::new(eye_ctr.x - pupil_radius,  (eye_ctr.y - pupil_radius/2) - HIGHLIGHT_Y_SHIFT );
//     let small_highlight_ctr = Point::new(eye_ctr.x + pupil_radius, (eye_ctr.y + pupil_radius/2) + HIGHLIGHT_Y_SHIFT);

//     // lens highlight large
//     let _ = Circle::with_center(highlight_ctr, highlight_diam_dim )
//         .into_styled(PrimitiveStyle::with_fill(Rgb565::WHITE))
//         .draw(display);

//     // lens highlight small
//     let _ = Circle::with_center(small_highlight_ctr, highlight_diam_dim/2) 
//         .into_styled(PrimitiveStyle::with_fill(Rgb565::WHITE))
//         .draw(display);

// }

fn draw_one_full_eye(frame_buf: &mut FullFrameBuf, look_correction: f32, pupil_ctr: &Point, pupil_diam: i32, iris_diam: i32,  iris_color: Rgb565, highlight_diam: i32) {
    let mut raw_fb =
        RawFrameBuf::<Rgb565, _>::new(frame_buf.as_mut_slice(), DISPLAY_WIDTH as usize, DISPLAY_HEIGHT as usize);
    // let crop_rect = Rectangle::with_center(*pupil_ctr, Size::new(DISPLAY_WIDTH as u32, (DISPLAY_HEIGHT as u32)/2));
    // let mut cropped_fb = raw_fb.cropped(&crop_rect);
    // draw_symmetric_inner_eye(&mut raw_fb, &pupil_ctr, iris_diam, pupil_diam, iris_color).unwrap();
    draw_elliptic_inner_eye(&mut raw_fb, &pupil_ctr, iris_diam, pupil_diam, iris_color, highlight_diam, look_correction).unwrap();
    // draw_asymmetric_inner_eye(&mut raw_fb, is_left , &pupil_ctr, pupil_diam, highlight_diam);
}



// ---- TASKS defined below ---

// TODO make this a real interrupt handler rather than parking waiting on falling edge?
#[embassy_executor::task]
async fn gpio_task(mut pin: Input<'static>) {
    loop {
        let mut mode_val = MODE_SETTING.load(Ordering::Relaxed);
        pin.wait_for_falling_edge().await;
        
        // Introduce a debounce delay
        Timer::after_millis(10).await; 

        if pin.is_low() {
            mode_val = (mode_val + 1) % NUM_A_MODES;
            MODE_SETTING.store(mode_val, Ordering::Relaxed);
        }
    }
}



type Spi1DeviceType = SpiDevice<'static, NoopRawMutex, Spi<'static, SPI1, Async>, Output<'static>>;

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // let mut pac = rp235x_pac::Peripherals::take().unwrap();
    let p = embassy_rp::init(Default::default());
    let total_fbuf_size = 2*FRAME_SIZE_BYTES ; //+ INNER_EYE_FBUF_SIZE_BYTES;
    info!("Start Config total_fbuf_size = {}",total_fbuf_size);

    MODE_SETTING.store(3, Ordering::Relaxed);

    let pin = Input::new(p.PIN_22, Pull::Up);
    unwrap!(spawner.spawn(gpio_task(pin)));


    
    let mut led = Output::new(p.PIN_25, Level::High);

    // Initialize frame buffers
    let disp0_frame_buf: &'static mut [u8; FRAME_SIZE_BYTES] = DISPLAY0_FRAMEBUF.init_with(move || [0; FRAME_SIZE_BYTES]);
    // let disp1_frame_buf: &'static mut [u8; FRAME_SIZE_BYTES] = DISPLAY1_FRAMEBUF.init_with(move || [0; FRAME_SIZE_BYTES]);

    // LCD display 0: ST7789V pins
    let bl0 = p.PIN_7; // --> BL
    let rst0 = p.PIN_6; // --> RST
    let dcx0 = p.PIN_5; // --> DC
    let cs0 = p.PIN_4; // SPI0 CSN --> CS
    let mosi0 = p.PIN_3; // SPI0 MosiPin --> DIN 
    let sck0 = p.PIN_2; // SPI0 SCK -->  CLK

    // // LCD display 1: ST7789V pins
    let bl1 = p.PIN_14;// --> BL
    let rst1 = p.PIN_13;// --> RST
    let dcx1 = p.PIN_12; // --> DC
    let mosi1 = p.PIN_11; // SPI1 MosiPin --> DIN
    let sck1 = p.PIN_10; // SPI1 SCK --> CLK
    let cs1 = p.PIN_9; // SPI1 CSN --> CS

    let mut display_config = spi::Config::default();
    display_config.frequency = DISPLAY_FREQ;
    display_config.phase = spi::Phase::CaptureOnSecondTransition;
    display_config.polarity = spi::Polarity::IdleHigh;


    // create SPI0
    let spi0: Spi<'_, embassy_rp::peripherals::SPI0, Async> = 
        Spi::new_txonly(p.SPI0, sck0, mosi0, p.DMA_CH0, display_config.clone());
    static SPI0_BUS: StaticCell<Mutex<NoopRawMutex, Spi<'static, embassy_rp::peripherals::SPI0, Async>>> = StaticCell::new();
    let spi0_bus = SPI0_BUS.init(Mutex::new(spi0));
    let spi0_device = SpiDevice::new(spi0_bus, Output::new(cs0, Level::High));

    // dcx: 0 = command, 1 = data
    let dcx0_out = Output::new(dcx0, Level::Low);
    let rst0_out = Output::new(rst0, Level::Low);

    // LCD backlight -- initially off
    let mut bl0_pwm_out: Pwm<'_> = Pwm::new_output_b(p.PWM_SLICE3, bl0, pwm::Config::default());

    // display interface abstraction from SPI and DC
    let spi_int0 = SpiInterface::new(spi0_device, dcx0_out);

    // Define the display from the display interface and initialize it
    let mut left_display = Builder::new(ST7789, spi_int0)
        .reset_pin(rst0_out)
        .display_size(DISPLAY_HEIGHT, DISPLAY_WIDTH)
        .orientation(Orientation::new().rotate(Rotation::Deg270))
        .invert_colors(ColorInversion::Inverted)
        .init(&mut Delay)
        .await
        .unwrap();


    // create SPI1
    let spi1: Spi<'_, embassy_rp::peripherals::SPI1, Async> = 
        Spi::new_txonly(p.SPI1, sck1, mosi1, p.DMA_CH1, display_config.clone());

    // // Create shared SPI1 bus
    // static SPI1_BUS: StaticCell<Mutex<NoopRawMutex, Spi<'static, embassy_rp::peripherals::SPI1, Async>>> = StaticCell::new();
    // let spi1_bus: &'static mut Mutex<NoopRawMutex, Spi<'static, SPI1, Async>> = SPI1_BUS.init(Mutex::new(spi1));
    // let spi1_device: Spi1DeviceType = SpiDevice::new(spi1_bus, Output::new(cs1, Level::High));

    // dcx: 0 = command, 1 = data
    let dcx1_out = Output::new(dcx1, Level::Low);
    let rst1_out = Output::new(rst1, Level::Low);

    // LCD backlight -- initially off
    // let mut bl1_pwm_out: Pwm<'static> = Pwm::new_output_a(p.PWM_SLICE7, bl1, pwm::Config::default());

    // display interface abstraction from SPI and DC
    // let spi_int1 = SpiInterface::new(spi1_device, dcx1_out);

    // Define the display from the display interface and initialize it
    // let right_display:RealDisplayType<embassy_rp::peripherals::SPI1> = Builder::new(ST7789, spi_int1)
    //     .reset_pin(rst1_out)
    //     .display_size(DISPLAY_HEIGHT, DISPLAY_WIDTH)
    //     .orientation(Orientation::new().rotate(Rotation::Deg90))
    //     .invert_colors(ColorInversion::Inverted)
    //     .init(&mut Delay)
    //     .await
    //     .unwrap();

    embassy_rp::multicore::spawn_core1(p.CORE1, unsafe { &mut CORE1_STACK },
        move || {
            let executor1 = EXECUTOR1.init(Executor::new());
            let bl1_pwm_out: Pwm<'static> = Pwm::new_output_a(p.PWM_SLICE7, bl1, pwm::Config::default());
            executor1.run(|spawner| spawner.spawn(core1_task( spi1, cs1, rst1_out, dcx1_out,  bl1_pwm_out)).unwrap());
        }
    );

    let eyeframe_left_qoi = Qoi::new(include_bytes!("../img/eye-frame-left-olive.qoi")).unwrap();
    let eyeframe_left_img: Image<'_, Qoi<'_>> = Image::new(&eyeframe_left_qoi, Point::new(0,0));
    // let eyeframe_left_alt_qoi = Qoi::new(include_bytes!("../img/eye-frame-left.qoi")).unwrap();
    // let _eyeframe_left_alt_img: Image<'_, Qoi<'_>> = Image::new(&eyeframe_left_alt_qoi, Point::new(0,0));

    // let eyeframe_right_qoi = Qoi::new(include_bytes!("../img/eye-frame-right-olive.qoi")).unwrap();
    // let eyeframe_right_img: Image<'_, Qoi<'_>> = Image::new(&eyeframe_right_qoi, Point::new(0,0));
    // let eyeframe_right_alt_qoi = Qoi::new(include_bytes!("../img/eye-frame-right.qoi")).unwrap();
    // let _eyeframe_right_alt_img: Image<'_, Qoi<'_>> = Image::new(&eyeframe_right_alt_qoi, Point::new(0,0));

    let iris_diam = IRIS_DIAM.into();
    let pupil_diam = PUPIL_DIAM.into();

    let mut iris_dirty = true ;
    let mut bg_dirty = true;
    let mut display_dirty = true;

    // Enable LCD backlight
    bl0_pwm_out.set_duty_cycle_percent(25).unwrap();
    // bl1_pwm_out.set_duty_cycle_percent(25).unwrap();

    let mut loop_count: usize = 0;
    let mut loop_elapsed_total: u64 = 0;
    let mut push_elapsed_total: u64 = 0;

    let mut rnd_src = embassy_rp::clocks::RoscRng;

    info!("Config done");

    let mut brightness_ascending: bool = true;
    let mut old_mode_val: usize = 555;

    // Main drawing loop, runs forever
    loop {
        led.set_low();
        let mode_val = MODE_SETTING.load(Ordering::Relaxed);
        let mut brightness_percent = CUR_BRIGHTNESS.load(Ordering::Relaxed);

        let loop_start_micros = Instant::now().as_micros();
        let mut rng_bytes:[u8;4] = [0; 4];
        rnd_src.fill_bytes(&mut rng_bytes);

        let mut look_step = rng_bytes[3] % 3;

         let iris_color: Rgb565 =
            if mode_val == 0 { 
                Rgb565::CSS_MAGENTA 
            }
            else if mode_val == 1 { 
                iris_dirty = true;
                Rgb565::CSS_RED 
            }
            else if mode_val == 2 {
                iris_dirty = true;
                let color_idx = loop_count % IRIS_PALETTE_SPECTRUM.len();
                Timer::after_millis(100).await;
                IRIS_PALETTE_SPECTRUM[color_idx]
            }
            else if mode_val == 3 {
                iris_dirty = true;
                // look_step = (loop_count % 3).try_into().unwrap();
                let color_idx = loop_count % IRIS_PALETTE_PURPLE.len();
                look_step = (color_idx % 3).try_into().unwrap();
                IRIS_PALETTE_PURPLE[color_idx]  
            }
            else {
                iris_dirty = true;
                Rgb565::new(rng_bytes[0],rng_bytes[1],rng_bytes[2])
             };
        

        CUR_IRIS_COLOR.store(iris_color.into_storage(), Ordering::Relaxed);

        if old_mode_val != mode_val {
            loop_count = 0;
            loop_elapsed_total = 0;
            push_elapsed_total = 0;
            old_mode_val = mode_val;
            iris_dirty = true;
            bg_dirty = true;
        }

        // TODO brightness cycling based on mode?
        if brightness_ascending {
            brightness_percent += 4;
            if brightness_percent >= 100 {
                brightness_percent = 100;
                brightness_ascending = false;
            }
        }
        else {
            brightness_percent -= 4;
            if brightness_percent == 0 { 
                brightness_percent = 5;
                brightness_ascending = true; 
            }
        }
        CUR_BRIGHTNESS.store(brightness_percent, Ordering::Relaxed);

        bl0_pwm_out.set_duty_cycle_percent(brightness_percent.try_into().unwrap()).unwrap();
        // bl1_pwm_out.set_duty_cycle_percent(brightness_percent).unwrap();

        if bg_dirty {
            // re-render the eye background images
            render_one_bg_image(disp0_frame_buf, &eyeframe_left_img);
            // render_one_bg_image(disp1_frame_buf, &eyeframe_right_img);
            display_dirty = true;
        }

        let look_correction = match look_step {
            1 => 0.95,
            2 => -0.95,
            _ => 1.0,
        };
  
        if iris_dirty {
            // Draw both eyes
            draw_one_full_eye(disp0_frame_buf, look_correction, &LEFT_PUPIL_CTR, pupil_diam, iris_diam, iris_color, HIGHLIGHT_DIAM);
            // draw_one_full_eye(disp1_frame_buf, look_correction, &right_pupil_ctr, pupil_diam, iris_diam, iris_color, highlight_diam);
            display_dirty = true;
        }

        let push_start_micros = Instant::now().as_micros();

        if display_dirty {
        // push both framebuffers to their respective displays
        left_display
            .show_raw_data(0, 0, 
                DISPLAY_WIDTH, DISPLAY_HEIGHT, 
                disp0_frame_buf)
            .await
            .unwrap();
        // right_display
        //     .show_raw_data(0, 0, 
        //         DISPLAY_WIDTH, DISPLAY_HEIGHT, 
        //         disp1_frame_buf)
        //     .await
        //     .unwrap();
        }

        led.set_high();

        iris_dirty = false;
        bg_dirty = false;
        if !display_dirty {
            Timer::after_millis(5).await;
        }
        display_dirty = false;

        loop_count += 1;
        let loop_finished_micros = Instant::now().as_micros();
        let loop_elapsed_micros = loop_finished_micros - loop_start_micros;
        let push_elapsed_micros = loop_finished_micros - push_start_micros;
        push_elapsed_total += push_elapsed_micros;
        loop_elapsed_total += loop_elapsed_micros;
        if loop_count % 100 == 0 {
            let avg_loop_elapsed = loop_elapsed_total / loop_count as u64;
            let avg_push_elapsed = push_elapsed_total / loop_count as u64;
            info!("avg_elapsed micros: {} {}", avg_push_elapsed, avg_loop_elapsed);
        }
    }
}


#[embassy_executor::task]
async fn core1_task(
    spi1: Spi<'static, SPI1, embassy_rp::spi::Async>,
    // spi1_device: Spi1DeviceType,
    // spi_int1:
    // SpiInterface<embassy_embedded_hal::shared_bus::asynch::spi::SpiDevice<'static, NoopRawMutex, embassy_rp::spi::Spi<'static, SPI1, embassy_rp::spi::Async>, Output<'static>>, Output<'static>>, 
    cs1: embassy_rp::Peri<'static,peripherals::PIN_9> , //Output<'static>,
    rst1_out: Output<'static>,
    dcx1_out: Output<'static>,
    // mut right_display: RealDisplayType<T>, 
    mut bl1_pwm_out:   Pwm<'static> ) {
    // let p = embassy_rp::init(Default::default());

    //     // LCD display 1: ST7789V pins
    // let bl1 = p.PIN_14;// --> BL
    // let rst1 = p.PIN_13;// --> RST
    // let dcx1 = p.PIN_12; // --> DC
    // let mosi1 = p.PIN_11; // SPI1 MosiPin --> DIN
    // let sck1 = p.PIN_10; // SPI1 SCK --> CLK
    // let cs1 = p.PIN_9; // SPI1 CSN --> CS

    // let mut display_config = spi::Config::default();
    // display_config.frequency = DISPLAY_FREQ;
    // display_config.phase = spi::Phase::CaptureOnSecondTransition;
    // display_config.polarity = spi::Polarity::IdleHigh;

    // // create SPI1
    // let spi1: Spi<'_, embassy_rp::peripherals::SPI1, Async> = 
    //     Spi::new_txonly(p.SPI1, sck1, mosi1, p.DMA_CH1, display_config.clone());

    // // Create shared SPI1 bus
    static SPI1_BUS: StaticCell<Mutex<NoopRawMutex, Spi<'static, embassy_rp::peripherals::SPI1, Async>>> = StaticCell::new();
    let spi1_bus = SPI1_BUS.init(Mutex::new(spi1));
    let spi1_device = SpiDevice::new(spi1_bus, Output::new(cs1, Level::High));

    // // dcx: 0 = command, 1 = data
    // let dcx1_out = Output::new(dcx1, Level::Low);
    // let rst1_out = Output::new(rst1, Level::Low);

    // // LCD backlight -- initially off
    // let mut bl1_pwm_out: Pwm<'_> = Pwm::new_output_a(p.PWM_SLICE7, bl1, pwm::Config::default());

    // // display interface abstraction from SPI and DC
    let spi_int1 = SpiInterface::new(spi1_device, dcx1_out);

    // Define the display from the display interface and initialize it
    let mut right_display = Builder::new(ST7789, spi_int1)
        .reset_pin(rst1_out)
        .display_size(DISPLAY_HEIGHT, DISPLAY_WIDTH)
        .orientation(Orientation::new().rotate(Rotation::Deg90))
        .invert_colors(ColorInversion::Inverted)
        .init(&mut Delay)
        .await
        .unwrap();

    // bl1_pwm_out.set_duty_cycle_percent(25).unwrap();

    // loop {
    //     bl1_pwm_out.set_duty_cycle_percent(25).unwrap();
    //     Timer::after_millis(300).await;
    //     bl1_pwm_out.set_duty_cycle_percent(75).unwrap();
    //     Timer::after_millis(300).await;
    // }

    let disp1_frame_buf: &'static mut [u8; FRAME_SIZE_BYTES] = DISPLAY1_FRAMEBUF.init_with(move || [0; FRAME_SIZE_BYTES]);

    let eyeframe_right_qoi = Qoi::new(include_bytes!("../img/eye-frame-right-olive.qoi")).unwrap();
    let eyeframe_right_img: Image<'_, Qoi<'_>> = Image::new(&eyeframe_right_qoi, Point::new(0,0));

    loop {
        let brightness_percent: u8 = CUR_BRIGHTNESS.load(Ordering::Relaxed).try_into().unwrap();
        bl1_pwm_out.set_duty_cycle_percent(brightness_percent).unwrap();

        let raw_iris_val = CUR_IRIS_COLOR.load(Ordering::Relaxed);
        let iris_color: Rgb565 = Rgb565::from(RawU16::new(raw_iris_val));

        render_one_bg_image(disp1_frame_buf, &eyeframe_right_img);

        draw_one_full_eye(disp1_frame_buf, 1.0, &RIGHT_PUPIL_CTR, PUPIL_DIAM.try_into().unwrap()
        , IRIS_DIAM.try_into().unwrap(), iris_color, HIGHLIGHT_DIAM);


        // Process data from SPI1
        right_display
            .show_raw_data(0, 0, 
                DISPLAY_WIDTH, DISPLAY_HEIGHT, 
                disp1_frame_buf)
            .await
            .unwrap();
        // bl1_pwm_out.set_duty_cycle_percent(25).unwrap();
        // Timer::after_millis(300).await;
    }
}