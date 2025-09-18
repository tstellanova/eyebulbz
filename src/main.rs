#![no_std]
#![no_main]

/// 
/// This embedded application draws eyeballs on small LCD screens.
/// 
/// reference: Moriyama, Xiao, Cohn 2004 "Meticulously Detailed Eye Model and Its Application to Analysis of Facial Image"
/// 

use {defmt_rtt as _, panic_probe as _};

use defmt::{info, warn, error, Format, unwrap};
// use embassy_rp::pio_programs::rotary_encoder::Direction;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embedded_graphics::primitives::StrokeAlignment;
use core::u8;
use core::{default::Default};
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU8, AtomicUsize, Ordering};


use embassy_embedded_hal::shared_bus::asynch::spi::SpiDevice;
use embassy_executor::{Spawner, Executor};
use embassy_rp:: {
    self as hal, block::ImageDef, gpio::{Input, Level, Output, Pull}, peripherals::{self, SPI0, SPI1}, pwm::{self, Pwm, SetDutyCycle}, spi::{self, Async, Spi},
};


use embassy_sync::{blocking_mutex::{raw::{NoopRawMutex}}, mutex::Mutex, pubsub::PubSubChannel};
use embassy_time::{Delay, Instant, Timer};

use embedded_graphics::{
    prelude::*,
    image::Image,
    pixelcolor::{raw::RawU16, Rgb565}, 
    primitives::{Polyline, PrimitiveStyle, PrimitiveStyleBuilder, Rectangle}
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


// example/src/main.rs
use closed_svg_path_proc::import_svg_paths;
use closed_svg_path::{ClosedPolygon,ScanlineIntersections};

// TODO move eyemodelz into a separate lib crate?
#[allow(dead_code)]
mod eyemodelz;
use crate::eyemodelz::*;

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


// const DISPLAY_FREQ_BASE: u32 = 60_000_000;
// const DISPLAY_FREQ_MAX: u32 = 80_000_000;
// const DISPLAY_FREQ: u32 = (DISPLAY_FREQ_MAX - DISPLAY_FREQ_BASE)/2 + DISPLAY_FREQ_BASE;
const DISPLAY_FREQ: u32 = 72_000_000; //apparently the fastest we can drive with short wire lengths

const ORIGIN_POINT:Point = Point::new(0, 0);
const DISPLAY_WIDTH: u16 =  320; 
const DISPLAY_HEIGHT: u16 = 240; 
const PIXEL_SIZE: u16 = 2; // RGB565 = 2 bytes per pixel
const FRAME_SIZE_BYTES: usize = DISPLAY_WIDTH as usize * DISPLAY_HEIGHT as usize * PIXEL_SIZE as usize;
type FullFrameBuf = [u8; FRAME_SIZE_BYTES];

#[derive(Clone, Copy, Debug, Eq, PartialEq, TryFromPrimitive, Format)]
#[repr(u8)]
enum TestModeA {
    ClockStar = 0,
    HStep = 1,
    VStep = 2,
    HSweep = 3,
    VSweep = 4,
    SurpriseHSweep = 5,
    Meander = 6,
    SlowRandMeander = 7,
    Randomize = 8,
    MaxCount
}

const INTERFRAME_DELAY_MILLIS:usize = 120;

const MAX_MODE_B_COUNT: u8 = GazeDirection::NUM_FULL_SWEEP_STEPS as u8;

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



#[link_section = ".core1_stack"]
static mut CORE1_STACK: Stack<4096> = Stack::new();
static EXECUTOR1: StaticCell<Executor> = StaticCell::new();

static DISPLAY0_FRAMEBUF: StaticCell<FullFrameBuf> = StaticCell::new();
static DISPLAY1_FRAMEBUF: StaticCell<FullFrameBuf> = StaticCell::new();

        

// == Cross-core signaling stuff below
static MODE_A_VALUE: AtomicU8 = AtomicU8::new(0);
static MODE_B_VALUE: AtomicU8 = AtomicU8::new(0);
static CUR_BRIGHTNESS_PCT: AtomicU8 = AtomicU8::new(50);
static CUR_IRIS_COLOR: AtomicU16 = AtomicU16::new(0x18ff);
static CUR_LOOK_STEP: AtomicU8 = AtomicU8::new(0);
static CUR_BG_DIRTY: AtomicBool = AtomicBool::new(true);
static CUR_IRIS_DIRTY: AtomicBool = AtomicBool::new(true);
static CUR_EMOTION: AtomicU8 = AtomicU8::new(EmotionExpression::Neutral as u8);
static CUR_GAZE_DIR: AtomicU8 = AtomicU8::new(GazeDirection::StraightAhead as u8);

// Static signals that can be shared between tasks
static EYE_DATA_READY_CHANNEL: PubSubChannel<embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex, usize, 4, 4, 1> = PubSubChannel::new();
static RIGHT_EYE_DONE_SIGNAL: Signal<CriticalSectionRawMutex, usize> = Signal::new();


static NEUTRAL_EYEBG_BYTES: &[u8] = include_bytes!("../img/gradient_bg.qoi");
const fn get_emotion_bg_bytes(emotion: EmotionExpression, is_left: bool) -> Option<&'static [u8]> {
    match (is_left, emotion) {
        // (true, EmotionExpression::Neutral) => Some(NEUTRAL_EYEBG_BYTES),
        (false, EmotionExpression::Neutral) => Some(NEUTRAL_EYEBG_BYTES),
        // (true, EmotionExpression::Surprise) => Some(include_bytes!("../img/eyebg-left-surprise.qoi")),
        // (false, EmotionExpression::Surprise) => Some(include_bytes!("../img/eyebg-right-surprise.qoi")),
        _ => None
    }
}



#[derive(Clone, Copy, Debug, Eq, PartialEq, TryFromPrimitive, Format)]
#[repr(u32)]
enum SvgFileId {
    EyeLeft,
    EyeRight,
    SvgFileIdCount
}

import_svg_paths!(EyeLeft, "img/eyestack-left-gen.svg");
import_svg_paths!(EyeRight, "img/eyestack-right-gen.svg");

type RealDisplayType<T>=lcd_async::Display<SpiInterface<SpiDevice<'static, NoopRawMutex, Spi<'static, T, embassy_rp::spi::Async>, Output<'static>>, Output<'static>>, ST7789, Output<'static>>;

// type Spi0CsnType = embassy_rp::Peri<'static,peripherals::PIN_4>;
// type Spi1CsnType = embassy_rp::Peri<'static,peripherals::PIN_9>;
type Spi0CsnType = embassy_rp::Peri<'static,peripherals::PIN_17>;
type Spi1CsnType = embassy_rp::Peri<'static,peripherals::PIN_13> ;


fn get_svg_path_by_id<'a>(file_id: SvgFileId, path_id: &'a str) -> Option<&'a ClosedPolygon<'a>> {
    match file_id {
        SvgFileId::EyeLeft => get_svg_path_by_id_file_EyeLeft(path_id),
        SvgFileId::EyeRight => get_svg_path_by_id_file_EyeRight(path_id),
        _ => { error!("Unknown path_id: {}", path_id); None }
    }
}

fn get_svg_path_by_id_checked<'a>(file_id: SvgFileId, path_id: &'a str) -> Option<&'a ClosedPolygon<'a>> {
    let check = get_svg_path_by_id(file_id, path_id);
    if check.is_none() {
        warn!("No path for  {}:{}", file_id, path_id);
    }
    check
}

/// Convert an RGB888 hex code (commonly used for defining colors) and convert to RGB565
fn hex_to_rgb565(hex_color: u32) -> Rgb565 {
    // Extract 8-bit R, G, B components
    let r_8bit = ((hex_color >> 16) & 0xFF) as u8;
    let g_8bit = ((hex_color >> 8) & 0xFF) as u8;
    let b_8bit = (hex_color & 0xFF) as u8;

    // Convert to 5-bit R, 6-bit G, 5-bit B for Rgb565
    let r_5bit = r_8bit >> 3; // Take the most significant 5 bits
    let g_6bit = g_8bit >> 2; // Take the most significant 6 bits
    let b_5bit = b_8bit >> 3; // Take the most significant 5 bits

    // Combine into a u16 and create Rgb565
    let rgb565_value = ((r_5bit as u16) << 11) | ((g_6bit as u16) << 5) | (b_5bit as u16);

    Rgb565::from(RawU16::new(rgb565_value))
}

fn render_one_bg_image<T>(
    frame_buf: &mut FullFrameBuf, 
    bg_img: &embedded_graphics::image::Image<'_, T>) 
    where T: ImageDrawable,  Rgb565: From<<T as embedded_graphics::image::ImageDrawable>::Color>
{      
    let mut raw_fb =
        RawFrameBuf::<Rgb565, _>::new(frame_buf.as_mut_slice(), DISPLAY_WIDTH as usize, DISPLAY_HEIGHT as usize);
    bg_img.draw(&mut raw_fb.color_converted()).unwrap(); 
}

/// Draw the asset defined by the id, gaze direction, and step.
/// This assumes that stepped gaze transitions always begin at GazeDirection::StraightAhead
/// and end at the provided gaze_direction. In order to perform a reverse transition
/// (from gaze_direction to StraightAhead), you need to recalculate look_step appropriately.
fn draw_stepped_asset(frame_buf: &mut FullFrameBuf, 
    file_id: SvgFileId, 
    id_prefix: &str, 
    gaze_direction: GazeDirection,
    look_step_idx: u8, 
    style: &PrimitiveStyle<Rgb565>) 
{
    let asset_id = stepped_asset_name(id_prefix, gaze_direction, look_step_idx);
    // info!("draw: {} {} {} > {}", id_prefix, gaze_direction, look_step_idx, &asset_id.as_str());
    draw_closed_poly(frame_buf, file_id, &asset_id, style);
}

/// Lookup the preloaded ClosedPolygon and then draw it into the buffer with the style provided.
fn draw_closed_poly(frame_buf: &mut FullFrameBuf, file_id: SvgFileId, path_id: &str, style: &PrimitiveStyle<Rgb565>) {
    if let Some(cpoly) = get_svg_path_by_id_checked(file_id,path_id) {
        let mut raw_fb =
            RawFrameBuf::<Rgb565, &mut [u8]>::new(frame_buf.as_mut_slice(), DISPLAY_WIDTH as usize, DISPLAY_HEIGHT as usize);
        let _ = cpoly.clone().into_styled(*style).draw(&mut raw_fb);
    }
}


// ---- TASKS defined below ---

const PUSHBUTTON_DEBOUNCE_DELAY:u64 = 20;
// TODO make this a real interrupt handler rather than parking waiting on falling edge?
#[embassy_executor::task]
async fn mode_a_button_task(mut pin: Input<'static>) {
    loop {
        pin.wait_for_falling_edge().await;
        
        // Introduce a debounce delay
        Timer::after_millis(PUSHBUTTON_DEBOUNCE_DELAY).await; 

        if pin.is_low() {
            let mut mode_a_val = MODE_A_VALUE.load(Ordering::Relaxed);
            mode_a_val = (mode_a_val + 1) %  TestModeA::MaxCount as u8;
            MODE_A_VALUE.store(mode_a_val, Ordering::Relaxed);
        }
    }
}

// TODO make this a real interrupt handler rather than parking waiting on falling edge?
#[embassy_executor::task]
async fn mode_b_button_task(mut pin: Input<'static>) {
    loop {
        pin.wait_for_falling_edge().await;
        
        // Introduce a debounce delay
        Timer::after_millis(PUSHBUTTON_DEBOUNCE_DELAY).await; 

        if pin.is_low() {
            let mut mode_b_val = MODE_B_VALUE.load(Ordering::Relaxed);
            mode_b_val = (mode_b_val + 1) % MAX_MODE_B_COUNT;
            MODE_B_VALUE.store(mode_b_val, Ordering::Relaxed);
        }
    }
}


#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // let mut pac = rp235x_pac::Peripherals::take().unwrap();
    let p = embassy_rp::init(Default::default());
    let total_fbuf_size = 2*FRAME_SIZE_BYTES ;
    info!("Start Config total_fbuf_size = {}",total_fbuf_size);

    // Read MSP at runtime
    let sp: u32 = cortex_m::register::msp::read();
    info!("Core0 main MSP  = {:#010x}", sp);

    // prep for reading mode change events
    MODE_A_VALUE.store(TestModeA::Meander as u8, Ordering::Relaxed);
    MODE_B_VALUE.store(GazeDirection::StraightAhead as u8, Ordering::Relaxed);

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
    warn!("display_freq: {}",DISPLAY_FREQ);

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

    
    embassy_rp::multicore::spawn_core1(p.CORE1, 
        unsafe { core::ptr::addr_of_mut!(CORE1_STACK).as_mut().unwrap() }, //safe because we touch this once
        move || {
            let executor1 = EXECUTOR1.init(Executor::new());
            let bl1_pwm_out: Pwm<'static> = Pwm::new_output_b(p.PWM_SLICE7, bl1, pwm::Config::default());
            executor1.run(|spawner| {
                spawner.spawn(core1_drawing_task(spi1, cs1, rst1_out, dcx1_out, bl1_pwm_out)).unwrap()
            });
        }
    );

    // spawn the core0 drawing task
    unwrap!(spawner.spawn(core0_drawing_task(spi0,cs0,rst0_out,dcx0_out,bl0_pwm_out)));

    // read mode button events
    unwrap!(spawner.spawn(mode_a_button_task(Input::new(p.PIN_4, Pull::Up))));
    unwrap!(spawner.spawn(mode_b_button_task(Input::new(p.PIN_8, Pull::Up))));

    let mut iris_dirty = false;
    let mut bg_dirty = true;

    let mut main_loop_count: usize = 0;
    let mut rnd_src = embassy_rp::clocks::RoscRng;

    info!("Config done");

    let mut brightness_ascending: bool = true;
    let mut old_mode_a_val  = TestModeA::MaxCount;
    let mut old_mode_b_val  = u8::MAX;
    let mut emotion_val; // = EmotionExpression::Neutral ;
    let mut cur_gaze_dir = GazeDirection::StraightAhead;

    let eye_redraw_data_ready_pub = EYE_DATA_READY_CHANNEL.publisher().unwrap();

    let mut look_step_idx = 0 ;

    // Main drawing loop, runs forever
    loop {
        led.set_high();
        let mode_a_val: TestModeA = MODE_A_VALUE.load(Ordering::Relaxed).try_into().unwrap();
        let mode_b_val = MODE_B_VALUE.load(Ordering::Relaxed);
        let mut brightness_percent = CUR_BRIGHTNESS_PCT.load(Ordering::Relaxed);

        let mut frame_render_gap_millis = INTERFRAME_DELAY_MILLIS;

        let iris_color: Rgb565;
        
        // Let the user manually adjust the gaze direction using the MODE_B button
        let mut freeze_gaze_dir = false;

        match mode_a_val {
            TestModeA::HStep | TestModeA::VStep => {
                iris_color = 
                    if mode_a_val ==  TestModeA::HStep { Rgb565::CSS_SLATE_GRAY } 
                    else { Rgb565::CSS_LIME_GREEN };
                emotion_val = EmotionExpression::Neutral;
                brightness_percent = 75; brightness_ascending = false;
                frame_render_gap_millis = INTERFRAME_DELAY_MILLIS * 2;
                freeze_gaze_dir = true;
            }
            TestModeA::HSweep => { 
                iris_color = Rgb565::CSS_DARK_TURQUOISE ;
                emotion_val = EmotionExpression::Neutral;
            }
            TestModeA::VSweep => {
                iris_color = Rgb565::CSS_GOLDENROD;
                emotion_val = EmotionExpression::Neutral;
            }
            TestModeA::SurpriseHSweep => {
                emotion_val = EmotionExpression::Surprise;
                brightness_percent = 90; brightness_ascending = true;
                let color_idx = main_loop_count % IRIS_PALETTE_PURPLE.len();
                iris_color = IRIS_PALETTE_PURPLE[color_idx] ;
            }
            TestModeA::Meander => {
                iris_color = hex_to_rgb565(0x405D80); 
                emotion_val = EmotionExpression::Neutral;
                brightness_percent = 75; brightness_ascending = false;
            }
            TestModeA::SlowRandMeander => {
                iris_color = Rgb565::CSS_CHOCOLATE;
                emotion_val = EmotionExpression::Neutral;
                brightness_percent = 75; brightness_ascending = false;
                frame_render_gap_millis = INTERFRAME_DELAY_MILLIS * 2;
            }
            TestModeA::ClockStar => {
                iris_color = Rgb565::CSS_DEEP_SKY_BLUE;
                emotion_val = EmotionExpression::Neutral;
                brightness_percent = 75; brightness_ascending = false;
                frame_render_gap_millis = INTERFRAME_DELAY_MILLIS * 2;
            }
            TestModeA::Randomize => {
                emotion_val = EmotionExpression::Neutral;
                frame_render_gap_millis = INTERFRAME_DELAY_MILLIS / 2;
                let mut rng_bytes:[u8;4] = [0; 4];
                rnd_src.fill_bytes(&mut rng_bytes);
                iris_color = Rgb565::new(rng_bytes[0],rng_bytes[1],rng_bytes[2]);
            }
            _ => { unreachable!() }
        }
    
        if old_mode_a_val != mode_a_val  {
            info!("mode_a old: {} new: {}", old_mode_a_val, mode_a_val);
            iris_dirty = true;
            bg_dirty = true;
            old_mode_a_val = mode_a_val;
        }

        if old_mode_b_val != mode_b_val {
            info!("mode_b old: {} new: {}", old_mode_b_val, mode_b_val);
            iris_dirty = true;
            old_mode_b_val = mode_b_val;
        }

        if !freeze_gaze_dir {
            (cur_gaze_dir, look_step_idx) = match mode_a_val {
                TestModeA::HSweep | TestModeA::SurpriseHSweep => {
                    GazeDirection::gaze_and_step_for_hsweep(main_loop_count)
                }
                TestModeA::VSweep => {
                    GazeDirection::gaze_and_step_for_vsweep(main_loop_count)
                }
                TestModeA::ClockStar => {
                    GazeDirection::gaze_and_step_for_sparse_star(main_loop_count)
                }
                TestModeA::Meander => {
                    GazeDirection::gaze_and_look_for_meander(main_loop_count)
                }
                TestModeA::SlowRandMeander | TestModeA::Randomize => {
                    let rand_count = embassy_rp::clocks::RoscRng::next_u8();
                    GazeDirection::gaze_and_look_for_meander(rand_count as usize)
                }
                _ => { unreachable!() }
            };
            iris_dirty = true;
        }
        else if iris_dirty { 
            // update gaze direction and step based on mode_b_val
            let sweep_count = (mode_b_val as usize) % GazeDirection::NUM_FULL_SWEEP_STEPS;
            (cur_gaze_dir, look_step_idx)  = 
                match mode_a_val {
                    TestModeA::VStep => { GazeDirection::gaze_and_step_for_vsweep(sweep_count) }
                    TestModeA::HStep => { GazeDirection::gaze_and_step_for_hsweep(sweep_count)}
                    _ => { unreachable!() }
                };
            info!("new m_b {} gaze: {} step: {}", mode_b_val, cur_gaze_dir, look_step_idx);
        }



        // 5000/480 = steps per ten seconds
        // 100 / num_steps = pct per step
        // TODO brightness cycling based on mode?
        //  100 / (10000 / frame_render_gap_millis);
        let brightstep_pct_raw =  100*frame_render_gap_millis / 5000; //fade pwm over this interval
        // info!("intergap_ms: {} brightstep_pct_raw: {} ",frame_render_gap_millis, brightstep_pct_raw);
        let brightstep_pct: u8 = u8::max(brightstep_pct_raw.try_into().unwrap(), 1); //fade pwm over this interval
        // info!("brightstep_pct: {}", brightstep_pct);

        if brightness_ascending {
            brightness_percent += brightstep_pct;
            if brightness_percent >= 100 {
                brightness_percent = 100;
                brightness_ascending = false;
            }
        }
        else {
            if brightness_percent < brightstep_pct { 
                brightness_percent = 5;
                brightness_ascending = true; 
            }
            else {
                brightness_percent -= brightstep_pct;
            }
        }

        // ship all the redraw config values
        // info!("emote: {} gaze: {} look_step: {}", emotion_val, cur_gaze_dir, look_step_idx);
        CUR_GAZE_DIR.store(cur_gaze_dir as u8, Ordering::Relaxed);
        CUR_EMOTION.store(emotion_val as u8, Ordering::Relaxed);
        CUR_LOOK_STEP.store(look_step_idx, Ordering::Relaxed);
        CUR_IRIS_COLOR.store(iris_color.into_storage(), Ordering::Relaxed);
        CUR_IRIS_DIRTY.store(iris_dirty, Ordering::Relaxed);
        CUR_BG_DIRTY.store(bg_dirty, Ordering::Relaxed);
        CUR_BRIGHTNESS_PCT.store(brightness_percent, Ordering::Relaxed);

        // At this point, all of the config data points required to re-render the frame have been calculated
        // and passed as atomics. Publish a message to start rendering.

        eye_redraw_data_ready_pub.publish(main_loop_count.try_into().unwrap()).await;

        led.set_low();
        main_loop_count += 1;

        // give some time to inter-task stuff
        Timer::after_millis(frame_render_gap_millis.try_into().unwrap()).await;
        // wait_for
        //RIGHT_EYE_DONE_SIGNAL.wait().await;

        bg_dirty = false;
        iris_dirty = false;

    }
}


/**
 * Performs the main redrawing for each eye
 */
async fn redraw_loop<T>(is_left: bool, mut backlight_pwm_out:   Pwm<'static>, mut display: RealDisplayType<T>) 
where T: embassy_rp::spi::Instance
{
    // "warm up" the backlight-- this only runs once after a restart
    backlight_pwm_out.set_duty_cycle_percent(25).unwrap();

    let disp_frame_buf: &'static mut [u8; FRAME_SIZE_BYTES] = 
        if is_left {
            DISPLAY0_FRAMEBUF.init_with(move || [0; FRAME_SIZE_BYTES])
        }
        else {
            DISPLAY1_FRAMEBUF.init_with(move || [0; FRAME_SIZE_BYTES])
        };
    
    let mut eyebg_qoi: Option<Qoi> = None;
    let mut display_dirty = true;

    let mut eye_ready_sub = EYE_DATA_READY_CHANNEL.subscriber().unwrap();
    let mut last_emotion_val = EmotionExpression::MaxCount;

    let mut redraw_loop_count: usize = 0;
    let mut loop_elapsed_total: u64 = 0;

    loop {
        // sync on eye parameters data ready
        let eye_ready_msg:embassy_sync::pubsub::WaitResult<usize> = eye_ready_sub.next_message().await; 
        match eye_ready_msg {
            embassy_sync::pubsub::WaitResult::Lagged(missed_count) => {
                warn!("Missed {} syncs", missed_count);
            },
            _ => {}
            // embassy_sync::pubsub::WaitResult:: Message(redraw_sync_count) => {
            //     //info!("redraw_sync_count {} ", redraw_sync_count);
            //     if (redraw_loop_count+1) != redraw_sync_count {
            //         warn!("loop_count {} redraw_sync_count {}", redraw_loop_count, redraw_sync_count);
            //     }
            //  }
        }
        let loop_start_micros = Instant::now().as_micros();

        let bg_dirty = CUR_BG_DIRTY.load(Ordering::Relaxed);
        let iris_dirty = CUR_IRIS_DIRTY.load(Ordering::Relaxed);
        let brightness_percent: u8 = CUR_BRIGHTNESS_PCT.load(Ordering::Relaxed);
        let emotion_val: EmotionExpression = CUR_EMOTION.load(Ordering::Relaxed).try_into().unwrap();
        let cur_gaze_dir: GazeDirection = CUR_GAZE_DIR.load(Ordering::Relaxed).try_into().unwrap();
        let look_step: u8 = CUR_LOOK_STEP.load(Ordering::Relaxed);
        let iris_color: Rgb565 = Rgb565::from(RawU16::new(CUR_IRIS_COLOR.load(Ordering::Relaxed)));
        let skin_color: Rgb565 = match emotion_val {
            EmotionExpression::Neutral => hex_to_rgb565(0x8EB34E), //mid-lightness of CSS_DARK_OLIVE_GREEN
            EmotionExpression::Surprise => Rgb565::CSS_ORANGE, 
            _ => Rgb565::CSS_BLUE_VIOLET,
        };

        if emotion_val != last_emotion_val {
            if let Some(src_bytes) = get_emotion_bg_bytes(emotion_val, is_left) {
                eyebg_qoi = Qoi::new(src_bytes).ok();
            } else { 
                eyebg_qoi = None; 
            }

            last_emotion_val = emotion_val;
        }

        /*
        Listing here the overlapping regions of the eye that 
         stack up and are visible to an observer, in part from
         reference: Moriyama, Xiao, Cohn 2004 "Meticulously Detailed Eye Model and Its Application to Analysis of Facial Image"
        Background:
         - surrounding skin
         - brow
        Eyeball:
         - sclera
         - iris -- we specify additional inner-eye sub-parts beyond reference
         - inner and outer corners - ref region5 and region6 
         - lower eyelid 
            - ref region2 and region3
            - ref curve5 infraorbital furrow
         - upper eyelid - ref region1
         The lids themselves can be though of as consisting of multiple parts:
         - bulge bright region
         - infraorbital furrow
        */

        if bg_dirty || display_dirty  {
            if let Some(ref qoi) = eyebg_qoi {
                // recreating the Image drawable each time has low overhead
                let bg_img = Image::new(qoi, ORIGIN_POINT);
                render_one_bg_image(disp_frame_buf, &bg_img);
            }
            else { // just set a background skincolor
                let mut raw_fb =
                    RawFrameBuf::<Rgb565, &mut [u8]>::new(disp_frame_buf.as_mut_slice(), DISPLAY_WIDTH as usize, DISPLAY_HEIGHT as usize);
                let _ = raw_fb.clear(skin_color); 
            }

            draw_background_shapes(is_left, cur_gaze_dir, emotion_val, skin_color, disp_frame_buf);
            display_dirty = true;
        }

        if iris_dirty || display_dirty  {
            draw_inner_eye_shapes(is_left, cur_gaze_dir, emotion_val, look_step, iris_color, disp_frame_buf);
            draw_eyeball_overlay_shapes(is_left, cur_gaze_dir, emotion_val, skin_color, disp_frame_buf);
            display_dirty = true;
        }

        if display_dirty {
            // blit frame buffer to display via SPI
            display
                .show_raw_data(0, 0, 
                    DISPLAY_WIDTH, DISPLAY_HEIGHT, 
                    disp_frame_buf)
                .await
                .unwrap();
            display_dirty = false;
        }
        // Now set the brightness to the desired level
        backlight_pwm_out.set_duty_cycle_percent(brightness_percent).unwrap();

        redraw_loop_count += 1;
        // synchronize left and right eye drawing
        if is_left {
            let right_eye_loop_count = RIGHT_EYE_DONE_SIGNAL.wait().await;
            // This breaks when the debugger is attached, for some reason
            if right_eye_loop_count != redraw_loop_count {
                warn!("loop_count left {} != right {}",redraw_loop_count, right_eye_loop_count);
            }
        }
        else {
            // right eye is done drawing to screen -- notify folks waiting on this
            RIGHT_EYE_DONE_SIGNAL.signal(redraw_loop_count);
        }

        let loop_finished_micros = Instant::now().as_micros();
        let loop_elapsed_micros = loop_finished_micros - loop_start_micros;
        loop_elapsed_total += loop_elapsed_micros;
        if redraw_loop_count % 1000 == 0 {
            let avg_loop_elapsed = loop_elapsed_total / redraw_loop_count as u64;
            info!("mainloop {} µs",avg_loop_elapsed);
        }
    }
}


fn draw_background_shapes(is_left: bool, _gaze_dir: GazeDirection, _emotion: EmotionExpression, _skin_color:Rgb565, frame_buf: &mut FullFrameBuf) 
{
    let start_micros = Instant::now().as_micros();
    let file_id = if is_left { SvgFileId::EyeLeft } else { SvgFileId::EyeRight };

    let brow_style = PrimitiveStyleBuilder::new()
        .fill_color( Rgb565::CSS_BLACK )
        .stroke_color(Rgb565::BLACK)
        .stroke_width(1)
        .stroke_alignment(StrokeAlignment::Center)
        .build();
    let test_ellipse_style  = PrimitiveStyleBuilder::new()
        .fill_color( Rgb565::CSS_LIGHT_GREEN )
        .stroke_color(Rgb565::BLACK)
        .build();
        
    if is_left {
        // TODO ensure that this ellipse is also reflected correctly on right eye
        draw_closed_poly(frame_buf, file_id, "grande_ellipse", &test_ellipse_style);
    }

    // The eyebrow covers a lot of area, so we don't want to redraw too often
    draw_closed_poly(frame_buf, file_id, "eyebrow", &brow_style);

    let _elapsed_micros = Instant::now().as_micros() - start_micros;
    info!("bg redraw {} {}µs", is_left, _elapsed_micros);

}



fn draw_inner_eye_shapes(is_left:bool, end_gaze_dir: GazeDirection, _emotion: EmotionExpression, look_step: u8, 
    iris_color: Rgb565, frame_buf: &mut FullFrameBuf) 
{
    static RUN_COUNT:AtomicUsize = AtomicUsize::new(0);
    static TOTAL_ELAPSED_MICROS:AtomicUsize = AtomicUsize::new(0);
    let start_micros = Instant::now().as_micros();
    let file_id = if is_left { SvgFileId::EyeLeft } else { SvgFileId::EyeRight };

    let darker_iris_color = adjust_lightness_rgb565(iris_color, FACTOR_DARKEN_10);
    let iris_style = PrimitiveStyleBuilder::new()
        .fill_color(iris_color)
        .stroke_color(darker_iris_color)
        .stroke_width(1) // TODO polyline redraw with stroke width > 1 is currently very slow-- why?
        .stroke_alignment(StrokeAlignment::Center)
        .build();
        
    // In our model, the sclera never changes. Other things draw over this.
    draw_closed_poly(frame_buf, file_id, "sclera", &PrimitiveStyle::with_fill(hex_to_rgb565(0xf4eed7)));
    
    draw_stepped_asset(frame_buf, file_id, "iris", end_gaze_dir, look_step, &iris_style);
    draw_stepped_asset(frame_buf, file_id, "iris_shadow_top", end_gaze_dir, look_step, &PrimitiveStyle::with_fill(darker_iris_color));
    draw_stepped_asset(frame_buf, file_id, "pupil", end_gaze_dir, look_step, &PrimitiveStyle::with_fill(Rgb565::BLACK));
    draw_stepped_asset(frame_buf, file_id, "glint_lg", end_gaze_dir, look_step, &PrimitiveStyle::with_fill(Rgb565::WHITE));
    draw_stepped_asset(frame_buf, file_id, "glint_sm", end_gaze_dir, look_step, &PrimitiveStyle::with_fill(Rgb565::WHITE));

    let _elapsed_micros:usize = (Instant::now().as_micros() - start_micros).try_into().unwrap();
    let total_elapsed = TOTAL_ELAPSED_MICROS.fetch_add(_elapsed_micros, Ordering::Relaxed);
    let total_runs = RUN_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if total_runs > 1000 {
        let total_elapsed = total_elapsed + _elapsed_micros;
        let avg_elapsed_micros = total_elapsed / total_runs;
        info!("inner redraw {}µs", avg_elapsed_micros);
        // reset benchmarker
        TOTAL_ELAPSED_MICROS.store(_elapsed_micros, Ordering::Relaxed);
        RUN_COUNT.store(1, Ordering::Relaxed);
    }

}

/**
 Draw shapes that overlay the eyeball (sclera and all) after drawing the iris &c.
 Some overlay parts are inspired by reference to Moriyama et al paper.        
  - inner (toward nose) and outer eye corners - ref region5 and region6 
  - lower eyelid
    - ref region2 and region3
    - ref curve5 infraorbital furrow
  - upper eyelid - ref region1

  The lids themselves can be though of as consisting of multiple parts:
  - bulge bright region
  - infraorbital furrow
 */
fn draw_eyeball_overlay_shapes(is_left:bool, _gaze_dir: GazeDirection, _emotion:EmotionExpression, skin_color:Rgb565, frame_buf: &mut FullFrameBuf) {
    static RUN_COUNT:AtomicUsize = AtomicUsize::new(0);
    static TOTAL_ELAPSED_MICROS:AtomicUsize = AtomicUsize::new(0);

    let start_micros = Instant::now().as_micros();
    let file_id = if is_left { SvgFileId::EyeLeft } else { SvgFileId::EyeRight };

    let upper_lid_skin = hex_to_rgb565(0x73369a); //TODO get this custom color elsewhere
    let upper_lid_shine_color= adjust_lightness_rgb565(upper_lid_skin, FACTOR_BRIGHTEN_20);
    let upper_lid_skin_darker = adjust_lightness_rgb565(upper_lid_skin, FACTOR_DARKEN_30);

    let slightly_brighter_skin = adjust_lightness_rgb565(skin_color, FACTOR_BRIGHTEN_20);
    let slightly_darker_skin = adjust_lightness_rgb565(skin_color, FACTOR_DARKEN_20);

    let upper_lid_shine_style = PrimitiveStyleBuilder::new()
        .fill_color(upper_lid_shine_color)
        .stroke_color(upper_lid_skin_darker)
        .stroke_width(2)
        .stroke_alignment(StrokeAlignment::Center)
        .build();

    let upper_lid_style = PrimitiveStyleBuilder::new()
        .fill_color(upper_lid_skin) 
        .build();

    let upper_lid_shadow_style = PrimitiveStyleBuilder::new()
        .fill_color(hex_to_rgb565(0x1d1c4f))
        .build();

    let lower_lid_bulge_style = PrimitiveStyleBuilder::new()
        .fill_color(slightly_darker_skin)
        .stroke_color(Rgb565::CSS_BLACK)
        .stroke_width(1)
        .stroke_alignment(StrokeAlignment::Center)
        .build();

    let lower_lid_shine_style = PrimitiveStyleBuilder::new()
        .fill_color(slightly_brighter_skin)
        .stroke_color(Rgb565::CSS_BLACK)
        .stroke_width(1)
        .stroke_alignment(StrokeAlignment::Center)
        .build();
 
    // if emotion == EmotionExpression::Surprise { //TODO handle emotions 

    // draw the entire upper eyelid "module"
    draw_closed_poly(frame_buf, file_id, "upper_lid_shadow_neutral", &upper_lid_shadow_style);
    // we paint the shine below the lid because we want a line width on top?
    draw_closed_poly(frame_buf, file_id, "upper_lid_shine_11", &upper_lid_shine_style);
    draw_closed_poly(frame_buf, file_id, "upper_lid_11", &upper_lid_style);

    // draw the entire lower eyelid "module"
    draw_closed_poly(frame_buf, file_id, "lower_lid_bulge_11", &lower_lid_bulge_style);
    draw_closed_poly(frame_buf, file_id, "lower_lid_shine_11", &lower_lid_shine_style);


    let _elapsed_micros:usize = (Instant::now().as_micros() - start_micros).try_into().unwrap();
    let total_elapsed = TOTAL_ELAPSED_MICROS.fetch_add(_elapsed_micros, Ordering::Relaxed);
    let total_runs = RUN_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if total_runs > 1000 {
        let total_elapsed = total_elapsed + _elapsed_micros;
        let avg_elapsed_micros = total_elapsed / total_runs;
        info!("overlay redraw {}µs", avg_elapsed_micros);
        // reset benchmarker
        TOTAL_ELAPSED_MICROS.store(_elapsed_micros, Ordering::Relaxed);
        RUN_COUNT.store(1, Ordering::Relaxed);
    }
}

#[embassy_executor::task]
async fn core0_drawing_task(
    spi_raw: Spi<'static, SPI0, embassy_rp::spi::Async>,
    cs_peri: Spi0CsnType , 
    rst_out: Output<'static>,
    dcx_out: Output<'static>,
    backlight_pwm_out: Pwm<'static> ) 
{

    // Read MSP at runtime
    let sp: u32 = cortex_m::register::msp::read();
    info!("Core0 draw MSP = {:#010x}", sp);

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
    backlight_pwm_out: Pwm<'static> ) 
{

    // Read MSP at runtime
    let sp: u32 = cortex_m::register::msp::read();
    info!("Core1 draw MSP = {:#010x}", sp);

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