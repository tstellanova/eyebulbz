#![no_std]
#![no_main]

use {defmt_rtt as _, panic_probe as _};

use defmt::*;
use core::{default::Default};//sync::atomic::{AtomicBool}
use portable_atomic::{AtomicBool, AtomicU16, AtomicU8, AtomicUsize, Ordering};

use embassy_embedded_hal::shared_bus::asynch::spi::SpiDevice;
use embassy_executor::{Spawner, Executor};
use embassy_rp:: {
    self as hal, block::ImageDef, gpio::{Input, Level, Output, Pull}, peripherals::{self, SPI0, SPI1}, pwm::{self, Pwm, SetDutyCycle}, spi::{self, Async, Spi},
};


use embassy_sync::{blocking_mutex::{raw::{CriticalSectionRawMutex, NoopRawMutex}}, mutex::Mutex, pubsub::PubSubChannel};
use embassy_time::{Delay, Instant, Timer};

use embedded_graphics::{
    image::Image, pixelcolor::{raw::RawU16, Rgb565}, prelude::{DrawTargetExt, *}, primitives::{Arc, Circle, Ellipse, Primitive, PrimitiveStyle, PrimitiveStyleBuilder, Rectangle, Styled}
};

use embassy_rp::multicore::{Stack};

use static_cell::StaticCell;

use lcd_async::{
    interface::SpiInterface,
    models::ST7789,
    options::{ColorInversion, Orientation, Rotation},
    raw_framebuf::RawFrameBuf,
    Builder,
};

use tinyqoi::Qoi;
use num_enum::TryFromPrimitive;

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

const MODE_A_COUNT: u8 = 5;
const MODE_B_COUNT: u8 = 5;

#[derive(Debug, Eq, PartialEq, TryFromPrimitive)]
#[repr(u8)]
enum EmotionExpression {
    Neutral = 0,
    Happy = 1,
    // Surprise = 2,
    // Thoughtful = 3,
    // Curious = 4,
    // Confused = 5,
    // Shy = 6,
    // Love = 7,
    // Trepidation = 8,
    MaxCount
}

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

static EXECUTOR1: StaticCell<Executor> = StaticCell::new();

static DISPLAY0_FRAMEBUF: StaticCell<FullFrameBuf> = StaticCell::new();
static DISPLAY1_FRAMEBUF: StaticCell<FullFrameBuf> = StaticCell::new();

// == Cross-core signaling stuff below
static MODE_A_VALUE: AtomicU8 = AtomicU8::new(0);
static MODE_B_VALUE: AtomicU8 = AtomicU8::new(0);
static CUR_BRIGHTNESS: AtomicUsize = AtomicUsize::new(50);
static CUR_IRIS_COLOR: AtomicU16 = AtomicU16::new(0);
static CUR_LOOK_STEP: AtomicU8 = AtomicU8::new(0);
static CUR_BG_DIRTY: AtomicBool = AtomicBool::new(true);
static CUR_IRIS_DIRTY: AtomicBool = AtomicBool::new(true);
static CUR_EMOTION: AtomicU8 = AtomicU8::new(EmotionExpression::Neutral as u8);

static EYE_READY_CHANNEL: PubSubChannel<embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex, u32, 4, 4, 1> = PubSubChannel::new();

type RealDisplayType<T>=lcd_async::Display<SpiInterface<SpiDevice<'static, NoopRawMutex, Spi<'static, T, embassy_rp::spi::Async>, Output<'static>>, Output<'static>>, ST7789, Output<'static>>;

// type Spi0CsnType = embassy_rp::Peri<'static,peripherals::PIN_4>;
// type Spi1CsnType = embassy_rp::Peri<'static,peripherals::PIN_9>;
type Spi0CsnType = embassy_rp::Peri<'static,peripherals::PIN_17>;
type Spi1CsnType = embassy_rp::Peri<'static,peripherals::PIN_13> ;

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
async fn mode_a_button_task(mut pin: Input<'static>) {
    loop {
        let mut mode_a_val = MODE_A_VALUE.load(Ordering::Relaxed);
        pin.wait_for_falling_edge().await;
        
        // Introduce a debounce delay
        Timer::after_millis(10).await; 

        if pin.is_low() {
            mode_a_val = (mode_a_val + 1) % MODE_A_COUNT;
            MODE_A_VALUE.store(mode_a_val, Ordering::Relaxed);
        }
    }
}

// TODO make this a real interrupt handler rather than parking waiting on falling edge?
#[embassy_executor::task]
async fn mode_b_button_task(mut pin: Input<'static>) {
    loop {
        let mut mode_b_val = MODE_B_VALUE.load(Ordering::Relaxed);
        pin.wait_for_falling_edge().await;
        
        // Introduce a debounce delay
        Timer::after_millis(10).await; 

        if pin.is_low() {
            mode_b_val = (mode_b_val + 1) % EmotionExpression::MaxCount as u8;
            MODE_B_VALUE.store(mode_b_val, Ordering::Relaxed);
        }
    }
}




#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // let mut pac = rp235x_pac::Peripherals::take().unwrap();
    let p = embassy_rp::init(Default::default());
    let total_fbuf_size = 2*FRAME_SIZE_BYTES ; //+ INNER_EYE_FBUF_SIZE_BYTES;
    info!("Start Config total_fbuf_size = {}",total_fbuf_size);

    // Read MSP at runtime
    let sp: u32 = cortex_m::register::msp::read();
    info!("Main MSP (Core0) = {:#010X}", sp);

    // prep for reading mode change events
    MODE_A_VALUE.store(1, Ordering::Relaxed);
    MODE_B_VALUE.store(0, Ordering::Relaxed);

    let mut led = Output::new(p.PIN_25, Level::High);

    // // LCD display 0: ST7789V pins, flavor: original SPI0 
    // let bl0 = p.PIN_7; // --> BL
    // let rst0 = p.PIN_6; // --> RST
    // let dcx0 = p.PIN_5; // --> DC
    // let cs0 = p.PIN_4; // SPI0 CSN --> CS
    // let sda0 = p.PIN_3; // SPI0 MosiPin --> DIN 
    // let sck0 = p.PIN_2; // SPI0 SCK -->  CLK

    // LCD display 0: ST7789V pins, flavor: SPI0 on opposite side of Pico-2 board from SPI1 
    let sck0 = p.PIN_18; // SPI0 SCK -->  SCL/CLK
    let sda0 = p.PIN_19; // SPI0 TX --> SDA/DIN 
    let rst0 = p.PIN_20; // --> RST
    let dcx0 = p.PIN_21; // --> DC
    let bl0 = p.PIN_16; // --> BL
    let cs0 = p.PIN_17; // SPI0 CSN --> CS

    // // LCD display 1: ST7789V pins, flavor: original SPI1
    // let bl1 = p.PIN_14;// --> BL
    // let rst1 = p.PIN_13;// --> RST
    // let dcx1 = p.PIN_12; // --> DC
    // let sda1 = p.PIN_11; // SPI1 TX --> SDA/DIN
    // let sck1 = p.PIN_10; // SPI1 SCK --> CLK
    // let cs1 = p.PIN_9; // SPI1 CSN --> CS

    // LCD display 1: ST7789V pins, flavori: linear SPI1
    let bl1 = p.PIN_15;// --> BL
    let dcx1 = p.PIN_14; // --> DC
    let cs1 = p.PIN_13; // SPI1 CSN --> CS
    let rst1 = p.PIN_12; // --> DC
    let sda1 = p.PIN_11; // SPI1 TX --> SDA/DIN
    let sck1 = p.PIN_10; // SPI1 SCK --> SCL/CLK

    let mut display_config = spi::Config::default();
    display_config.frequency = DISPLAY_FREQ;
    display_config.phase = spi::Phase::CaptureOnSecondTransition;
    display_config.polarity = spi::Polarity::IdleHigh;


    let spi0: Spi<'_, embassy_rp::peripherals::SPI0, Async> = 
        Spi::new_txonly(p.SPI0, sck0, sda0, p.DMA_CH0, display_config.clone());
    let dcx0_out = Output::new(dcx0, Level::Low);
    let rst0_out = Output::new(rst0, Level::Low);
    // let bl0_pwm_out: Pwm<'_> = Pwm::new_output_b(p.PWM_SLICE3, bl0, pwm::Config::default());//TODO doesn't work?
    let bl0_pwm_out: Pwm<'_> = Pwm::new_output_a(p.PWM_SLICE0, bl0, pwm::Config::default());//TODO doesn't work?

    let spi1: Spi<'_, embassy_rp::peripherals::SPI1, Async> = 
        Spi::new_txonly(p.SPI1, sck1, sda1, p.DMA_CH1, display_config.clone());
    let dcx1_out = Output::new(dcx1, Level::Low);
    let rst1_out = Output::new(rst1, Level::Low);

    embassy_rp::multicore::spawn_core1(p.CORE1, unsafe { &mut CORE1_STACK },
        move || {
            let executor1 = EXECUTOR1.init(Executor::new());
            // let bl1_pwm_out: Pwm<'static> = Pwm::new_output_a(p.PWM_SLICE7, bl1, pwm::Config::default());
            let bl1_pwm_out: Pwm<'static> = Pwm::new_output_b(p.PWM_SLICE7, bl1, pwm::Config::default());
            executor1.run(|spawner| spawner.spawn(core1_drawing_task( spi1, cs1, rst1_out, dcx1_out,  bl1_pwm_out)).unwrap());
        }
    );

    // spawn the core0 drawing task
    unwrap!(spawner.spawn(core0_drawing_task(spi0,cs0,rst0_out,dcx0_out,bl0_pwm_out)));

    // read Mode A button events
    // unwrap!(spawner.spawn(mode_a_button_task(Input::new(p.PIN_22, Pull::Up))));
    unwrap!(spawner.spawn(mode_a_button_task(Input::new(p.PIN_4, Pull::Up))));
    unwrap!(spawner.spawn(mode_b_button_task(Input::new(p.PIN_8, Pull::Up))));

    let mut iris_dirty = true ;
    let mut bg_dirty = true;

    let mut loop_count: usize = 0;
    let mut loop_elapsed_total: u64 = 0;
    let mut recalc_elapsed_total: u64 = 0;

    let mut rnd_src = embassy_rp::clocks::RoscRng;

    info!("Config done");

    let mut brightness_ascending: bool = true;
    let mut old_mode_a_val  = 255;
    let mut old_mode_b_val  = 255;

    let eye_ready_pub = EYE_READY_CHANNEL.publisher().unwrap();

    // Main drawing loop, runs forever
    loop {
        led.set_low();
        let mode_a_val = MODE_A_VALUE.load(Ordering::Relaxed);
        let mode_b_val = MODE_B_VALUE.load(Ordering::Relaxed);

        let mut brightness_percent = CUR_BRIGHTNESS.load(Ordering::Relaxed);

        let loop_start_micros = Instant::now().as_micros();

        let mut look_step = (loop_count % 3).try_into().unwrap();
        let mut frame_render_gap_usec = 100;


        if old_mode_b_val != mode_b_val {
            info!("old mode_b: {} new: {}", old_mode_b_val, mode_b_val);
            loop_count = 0;
            loop_elapsed_total = 0;
            recalc_elapsed_total = 0;
            iris_dirty = true;
            bg_dirty = true;
            old_mode_b_val = mode_b_val;
            // TODO for development, emotion is locked to Mode B state
            CUR_EMOTION.store(mode_b_val, Ordering::Relaxed);
        }


        let iris_color: Rgb565 =
            if mode_a_val == 0 { 
                frame_render_gap_usec = 500;
                Rgb565::CSS_MAGENTA 
            }
            else if mode_a_val == 1 { 
                frame_render_gap_usec = 100;
                iris_dirty = true;
                Rgb565::CSS_RED 
            }
            else if mode_a_val == 2 {
                iris_dirty = true;
                let color_idx = loop_count % IRIS_PALETTE_SPECTRUM.len();
                IRIS_PALETTE_SPECTRUM[color_idx]
            }
            else if mode_a_val == 3 {
                frame_render_gap_usec = 200;
                iris_dirty = true;
                let color_idx = loop_count % IRIS_PALETTE_PURPLE.len();
                IRIS_PALETTE_PURPLE[color_idx]  
            }
            else {
                frame_render_gap_usec = 250;
                let mut rng_bytes:[u8;4] = [0; 4];
                rnd_src.fill_bytes(&mut rng_bytes);
                look_step = ( rng_bytes[3] % 3).try_into().unwrap();
                iris_dirty = true;
                Rgb565::new(rng_bytes[0],rng_bytes[1],rng_bytes[2])
            };
    
        // TODO we keep mode A and mode B separate for now-- eg Mode B might be inner-eye specific at some point
        if old_mode_a_val != mode_a_val  {
            loop_count = 0;
            loop_elapsed_total = 0;
            recalc_elapsed_total = 0;
            iris_dirty = true;
            bg_dirty = true;
            old_mode_a_val = mode_a_val;
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

        // ship all the redraw config values
        CUR_LOOK_STEP.store(look_step, Ordering::Relaxed);
        CUR_IRIS_COLOR.store(iris_color.into_storage(), Ordering::Relaxed);
        CUR_IRIS_DIRTY.store(iris_dirty, Ordering::Relaxed);
        CUR_BG_DIRTY.store(bg_dirty, Ordering::Relaxed);
        CUR_BRIGHTNESS.store(brightness_percent, Ordering::Relaxed);

        // At this point, all of the config data points required to re-render the frame have been calculated
        // and passed as atomics. Publish a message to start rendering.

        eye_ready_pub.publish(loop_count.try_into().unwrap()).await;
        // give some time to inter-task stuff
        Timer::after_millis(frame_render_gap_usec).await;

        let push_start_micros = Instant::now().as_micros();
        let loop_finished_micros: u64 = Instant::now().as_micros();
        led.set_high();

        iris_dirty = false;
        bg_dirty = false;

        loop_count += 1;
        let loop_elapsed_micros = loop_finished_micros - loop_start_micros;
        let recalc_frame_micros = loop_finished_micros - push_start_micros;
        recalc_elapsed_total += recalc_frame_micros;
        loop_elapsed_total += loop_elapsed_micros;
        if loop_count % 100 == 0 {
            let avg_loop_elapsed = loop_elapsed_total / loop_count as u64;
            let avg_recalc_elapsed = recalc_elapsed_total / loop_count as u64;
            info!("avg_elapsed micros: {} {}", avg_recalc_elapsed, avg_loop_elapsed);
        }
    }
}


/**
 * Performs the main redrawing for each eye
 */
async fn redraw_loop<T>(is_left: bool, mut backlight_pwm_out:   Pwm<'static>, mut display: RealDisplayType<T>) 
where T: embassy_rp::spi::Instance

{
    backlight_pwm_out.set_duty_cycle_percent(25).unwrap();

    let disp_frame_buf: &'static mut [u8; FRAME_SIZE_BYTES] = 
        if is_left {
            DISPLAY0_FRAMEBUF.init_with(move || [0; FRAME_SIZE_BYTES])
        }
        else {
            DISPLAY1_FRAMEBUF.init_with(move || [0; FRAME_SIZE_BYTES])
        };

    // init a set of Qoi images to read from flash (.rodata)
    let bgi_neutral_qoi = {
        if is_left { Qoi::new(include_bytes!("../img/eye-frame-left-neut.qoi")).unwrap()}
        else { Qoi::new(include_bytes!("../img/eye-frame-right-neut.qoi")).unwrap() }
    };
    let bgi_happy_qoi = {
        if is_left { Qoi::new(include_bytes!("../img/eye-frame-left-happy.qoi")).unwrap()}
        else { Qoi::new(include_bytes!("../img/eye-frame-right-happy.qoi")).unwrap() }
    };

    let mut eyeframe_bg_img =  Image::new(&bgi_neutral_qoi, Point::new(0,0));
    let mut display_dirty = true;
    let pupil_ctr_val = {
        if is_left { LEFT_PUPIL_CTR }
        else { RIGHT_PUPIL_CTR}
    };

    let mut eye_ready_sub = EYE_READY_CHANNEL.subscriber().unwrap();
    let mut last_emotion_val = EmotionExpression::Neutral;

    loop {
        // sync on eye parameters data ready
        let _eye_ready_msg = eye_ready_sub.next_message_pure().await;
        let bg_dirty = CUR_BG_DIRTY.load(Ordering::Relaxed);
        let iris_dirty = CUR_IRIS_DIRTY.load(Ordering::Relaxed);
        let brightness_percent: u8 = CUR_BRIGHTNESS.load(Ordering::Relaxed).try_into().unwrap();
        let emotion_val: EmotionExpression = CUR_EMOTION.load(Ordering::Relaxed).try_into().unwrap();
        backlight_pwm_out.set_duty_cycle_percent(brightness_percent).unwrap();

        let look_step = CUR_LOOK_STEP.load(Ordering::Relaxed);
        let look_correction = match look_step {
            1 => 0.95,
            2 => -0.95,
            _ => 1.0,
        };

        let iris_color: Rgb565 = Rgb565::from(RawU16::new(CUR_IRIS_COLOR.load(Ordering::Relaxed)));

        if emotion_val != last_emotion_val {
            eyeframe_bg_img =
                match emotion_val {
                    EmotionExpression::Happy => {
                        Image::new(&bgi_happy_qoi, Point::new(0,0))
                    },
                    _ => {
                        Image::new(&bgi_neutral_qoi, Point::new(0,0))
                    }
                };

            last_emotion_val = emotion_val;
        }

        if bg_dirty || display_dirty {
            render_one_bg_image(disp_frame_buf, &eyeframe_bg_img);
            display_dirty = true;
        }

        if iris_dirty || display_dirty {
            draw_one_full_eye(disp_frame_buf, look_correction, &pupil_ctr_val, PUPIL_DIAM.try_into().unwrap()
            , IRIS_DIAM.try_into().unwrap(), iris_color, HIGHLIGHT_DIAM);
            display_dirty = true;
        }

        if display_dirty {
            // Process data from SPI1
            display
                .show_raw_data(0, 0, 
                    DISPLAY_WIDTH, DISPLAY_HEIGHT, 
                    disp_frame_buf)
                .await
                .unwrap();
            display_dirty = false;
        }

    }
}


#[embassy_executor::task]
async fn core0_drawing_task(
    spi_raw: Spi<'static, SPI0, embassy_rp::spi::Async>,
    cs_peri: Spi0CsnType , 
    rst_out: Output<'static>,
    dcx_out: Output<'static>,
    backlight_pwm_out: Pwm<'static> ) {

    // Read MSP at runtime
    let sp: u32 = cortex_m::register::msp::read();
    info!("Core0 MSP = {:#010X}", sp);

    // Create shared SPI1 bus from raw Spi
    static SPI_BUS: StaticCell<Mutex<NoopRawMutex, Spi<'static, SPI0, Async>>> = StaticCell::new();
    let spi_bus = SPI_BUS.init(Mutex::new(spi_raw));
    let spi_device = SpiDevice::new(spi_bus, Output::new(cs_peri, Level::High));
    // display interface abstraction from SPI and DC
    let spi_int = SpiInterface::new(spi_device, dcx_out);

    // Define the display from the display interface and initialize it
    let display = Builder::new(ST7789, spi_int)
        .reset_pin(rst_out)
        .display_size(DISPLAY_HEIGHT, DISPLAY_WIDTH)
        .orientation(Orientation::new().rotate(Rotation::Deg270))
        .invert_colors(ColorInversion::Inverted)
        .init(&mut Delay)
        .await
        .unwrap();

    redraw_loop(true, backlight_pwm_out, display).await;

}

#[embassy_executor::task]
async fn core1_drawing_task(
    spi_raw: Spi<'static, SPI1, embassy_rp::spi::Async>,
    cs_peri: Spi1CsnType , 
    rst_out: Output<'static>,
    dcx_out: Output<'static>,
    backlight_pwm_out: Pwm<'static> ) {

    // Read MSP at runtime
    let sp: u32 = cortex_m::register::msp::read();
    info!("Core1 MSP = {:#010X}", sp);

    // Create shared SPI1 bus from raw Spi
    static SPI_BUS: StaticCell<Mutex<NoopRawMutex, Spi<'static, SPI1, Async>>> = StaticCell::new();
    let spi_bus = SPI_BUS.init(Mutex::new(spi_raw));
    let spi_device = SpiDevice::new(spi_bus, Output::new(cs_peri, Level::High));
    // display interface abstraction from SPI and DC
    let spi_int = SpiInterface::new(spi_device, dcx_out);

    // Define the display from the display interface and initialize it
    let  display = Builder::new(ST7789, spi_int)
        .reset_pin(rst_out)
        .display_size(DISPLAY_HEIGHT, DISPLAY_WIDTH)
        .orientation(Orientation::new().rotate(Rotation::Deg90))
        .invert_colors(ColorInversion::Inverted)
        .init(&mut Delay)
        .await
        .unwrap();

    redraw_loop(false, backlight_pwm_out, display).await;

}