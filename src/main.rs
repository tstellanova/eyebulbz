#![no_std]
#![no_main]


use defmt::*;
use core::default::Default;
use portable_atomic::{AtomicUsize, Ordering};

use embassy_embedded_hal::shared_bus::asynch::spi::SpiDevice;
use embassy_executor::{Spawner};
use embassy_rp:: {
    gpio::{Input, Level, Output, Pull}, pwm::{self, Pwm, SetDutyCycle}, spi::{self, Async, Spi}
};

use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_time::{Delay, Timer};

use embedded_graphics::{
    prelude::DrawTargetExt,
    image::Image, pixelcolor::Rgb565, prelude::*, 
    primitives::{Arc, Circle, Primitive, PrimitiveStyle, Styled}
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

const DISPLAY_WIDTH: u16 = 320;
const DISPLAY_HEIGHT: u16 = 240;
const PIXEL_SIZE: u16 = 2; // RGB565 = 2 bytes per pixel
const FRAME_SIZE_BYTES: usize = DISPLAY_WIDTH as usize * DISPLAY_HEIGHT as usize * PIXEL_SIZE as usize;
type FullFrameBuf = [u8; FRAME_SIZE_BYTES];
static SINGLE_FRAMEBUF: StaticCell<FullFrameBuf> = StaticCell::new();

const INNER_EYE_FBUF_W: u16 = 125;
const INNER_EYE_FBUF_H: u16 = 125;
const INNER_EYE_FBUF_SIZE_BYTES: usize = INNER_EYE_FBUF_W as usize * INNER_EYE_FBUF_H as usize * PIXEL_SIZE as usize;
static IRIS_FRAMEBUF: StaticCell<[u8; INNER_EYE_FBUF_SIZE_BYTES]> = StaticCell::new();

static MODE_SETTING: AtomicUsize = AtomicUsize::new(0);

type RealDisplayType<T>=lcd_async::Display<SpiInterface<SpiDevice<'static, NoopRawMutex, Spi<'static, T, embassy_rp::spi::Async>, Output<'static>>, Output<'static>>, ST7789, Output<'static>>;


// TODO make this a real interrupt handler rather than parking waiting on falling edge?
#[embassy_executor::task]
async fn gpio_task(mut pin: Input<'static>) {
    loop {
        let mut mode_val = MODE_SETTING.load(Ordering::Relaxed);
        pin.wait_for_falling_edge().await;
        
        // Introduce a debounce delay
        Timer::after_millis(10).await; 

        if pin.is_low() {
            mode_val = (mode_val + 1) % NUM_MODES;
            MODE_SETTING.store(mode_val, Ordering::Relaxed);
        }
    }
}



fn copy_centered_region_chunks(
    src_buffer: &[u8],
    dest_buffer: &mut [u8],
    display_width: usize,
    inner_width: usize,
    inner_height: usize,
    center: &Point, 
) {

    const BYTES_PER_PIXEL: usize = 2;
    let start_x = (center.x as usize).saturating_sub(inner_width / 2);
    let start_y = (center.y as usize).saturating_sub(inner_height / 2);

    let src_rows = src_buffer.chunks_exact(display_width * BYTES_PER_PIXEL);
    let dest_rows = dest_buffer.chunks_exact_mut(inner_width * BYTES_PER_PIXEL);

    for (dest_row, src_row) in dest_rows.zip(src_rows.skip(start_y).take(inner_height)) {
        let src_start = start_x * BYTES_PER_PIXEL;
        let src_end = src_start + inner_width * BYTES_PER_PIXEL;
        dest_row.copy_from_slice(&src_row[src_start..src_end]);
    }

}


async fn redraw_symmetric_bg<T,U>(left_display: &mut RealDisplayType<T>, right_display: &mut RealDisplayType<U>, 
    base_frame_buf: &mut FullFrameBuf, 
    bg_img: &Image<'_, Qoi<'_>>) 
    where T: embassy_rp::spi::Instance, 
    U: embassy_rp::spi::Instance,
{       
    // draw the symmetric background image into the right display, then mirror into the left
    let mut raw_fb =
        RawFrameBuf::<Rgb565, _>::new(base_frame_buf.as_mut_slice(), DISPLAY_WIDTH as usize, DISPLAY_HEIGHT as usize);
    // raw_fb.clear(Rgb565::BLACK).unwrap();
    bg_img.draw(&mut raw_fb.color_converted()).unwrap(); 
    let _ = right_display
        .show_raw_data(0, 0, 
            DISPLAY_WIDTH.try_into().unwrap(), DISPLAY_HEIGHT.try_into().unwrap(), 
            base_frame_buf)
            .await.unwrap();

    // temporarily flip the alternate display orientation so that we can mirror the symmetric background image
    let orig_left_orient = left_display.orientation();
    let tmp_left_orient = orig_left_orient.flip_horizontal();
    left_display.set_orientation(tmp_left_orient).await.unwrap();
    let _ = left_display
        .show_raw_data(0, 0, 
            DISPLAY_WIDTH.try_into().unwrap(), DISPLAY_HEIGHT.try_into().unwrap(), 
            base_frame_buf)
            .await.unwrap();
    // restore the alternate display orientation
    left_display.set_orientation(orig_left_orient).await.unwrap();
    
}

const FARPOINT_CENTER: Point = Point::new(160, 240);
// const EYEBROW_DIAMETER: u32 = 470u32;
// const EYELID_TOP_DIAMETER: u32 = 330u32;
const EYELASH_DIAMETER: u32 = 310u32;

// const RT_PUPIL_CTR: Point = Point::new(148,159);
// const LID_SHADOW_CTR_RT: Point = Point::new(RT_PUPIL_CTR.x, RT_PUPIL_CTR.y + 10);


fn make_styled_arc(center: Point, diam: u32, start_deg: f32, sweep_deg: f32, color: Rgb565, stroke_width: u32) -> Styled<Arc, PrimitiveStyle<Rgb565>> {
    Styled::new(
        Arc::with_center(center, 
            diam, 
            Angle::from_degrees(start_deg), 
        Angle::from_degrees(sweep_deg)),
        PrimitiveStyle::with_stroke(color, stroke_width),
    )
}



// fn draw_symm_outer_eye<T>(
//     display: &mut T, 
//     _is_left: bool, 
// ) -> Result<(), T::Error>
// where
//     T: DrawTarget<Color = Rgb565>,
// {
//     // this line appears vertical onscreen
//     // Line::new(Point::new(160, 0), Point::new(160, 240))
//     //     .into_styled(PrimitiveStyle::with_stroke(Rgb565::GREEN, 1))
//     //     .draw(display)?;

//     // this line appears horizontal onscreen
//     //  Line::new(Point::new(0, 120), Point::new(320, 120))
//     //     .into_styled(PrimitiveStyle::with_stroke(Rgb565::BLUE, 1))
//     //     .draw(display)?;

//     // draw stuff that doesn't shade iris
//     // eyebrow
//     make_styled_arc(FARPOINT_CENTER, EYEBROW_DIAMETER, 
//         -55.0, -75.0, Rgb565::CSS_BLUE_VIOLET, 14).draw(display)?;
//     // top eyelid
//     make_styled_arc(FARPOINT_CENTER - Size::new(0, 10), EYELID_TOP_DIAMETER-10, 
//         -60.0, -60.0, Rgb565::BLACK, 4).draw(display)?;
//     make_styled_arc(FARPOINT_CENTER + Size::new(0, 5), EYELID_TOP_DIAMETER+20, 
//         -60.0, -60.0, Rgb565::BLACK, 3).draw(display)?;

//     Ok(())
// }

fn draw_symmetric_inner_eye<T>(
    display: &mut T, 
    pupil_ctr: &Point, 
    iris_diam: i32, 
    pupil_diam: i32, 
    iris_color: Rgb565) -> Result<(), T::Error>
where
    T: DrawTarget<Color = Rgb565>,
{   
    let pupil_diam_dim: u32 = pupil_diam.try_into().unwrap();
    let iris_diam_dim: u32 = iris_diam.try_into().unwrap();

    // behind iris
    Circle::with_center(*pupil_ctr, iris_diam_dim + 4)
        .into_styled(PrimitiveStyle::with_fill(Rgb565::BLACK))
        .draw(display)?;

    // iris
    Circle::with_center(*pupil_ctr, iris_diam_dim)
        .into_styled(PrimitiveStyle::with_fill(iris_color))
        .draw(display)?;

    // // eyelid shadow (over iris)
    // make_styled_arc(*pupil_ctr, pupil_diam_dim + 20, 
    //     -30.0, -120.0, Rgb565::BLACK, 40).draw(display)?;
    
    // pupil
    Circle::with_center(*pupil_ctr, pupil_diam_dim )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::BLACK))
        .draw(display)?;

    // draw stuff that shades iris
    // eyelash inner (liner)
    make_styled_arc(FARPOINT_CENTER + Size::new(0,30), EYELASH_DIAMETER+30, 
        -45.0, -90.0, Rgb565::CYAN, 8).draw(display)?;

    // eyelash outer
    make_styled_arc(FARPOINT_CENTER, EYELASH_DIAMETER, 
        -35.0, -110.0, Rgb565::CSS_INDIGO, 12).draw(display)?;

    Ok(())
}


fn draw_asymmetric_inner_eye<T>(
    display: &mut T, 
    _is_left: bool, 
    eye_ctr: &Point,  
    pupil_diam: i32, 
    highlight_diam: i32,
)
where
    T: DrawTarget<Color = Rgb565>,
{   
    let pupil_radius = pupil_diam / 2;
    let highlight_diam_dim: u32 = highlight_diam.try_into().unwrap();

    // two highlights are symmetric about the pupil center
    let highlight_ctr = Point::new(eye_ctr.x - pupil_radius,  eye_ctr.y - pupil_radius/2  );
    let small_highlight_ctr = Point::new(eye_ctr.x + pupil_radius, eye_ctr.y + pupil_radius/2 );

    // lens highlight large
    let _ = Circle::with_center(highlight_ctr, highlight_diam_dim )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::WHITE))
        .draw(display);

    // lens highlight small
    let _ = Circle::with_center(small_highlight_ctr, highlight_diam_dim/2) 
        .into_styled(PrimitiveStyle::with_fill(Rgb565::WHITE))
        .draw(display);

}

// // Flip a buffer representing an Rgb565 image horizontally (about Y axis)
// fn fliph_rgb565_inplace(buffer: &mut [u8], width: usize, height: usize) {
//     crate::assert_eq!(buffer.len(), width * height * 2);
    
//     // Reinterpret buffer as 16-bit pixels
//     let pixels = unsafe {
//         core::slice::from_raw_parts_mut(buffer.as_mut_ptr() as *mut u16, buffer.len() / 2)
//     };

//     for row in pixels.chunks_exact_mut(width) {
//         let mut left = 0;
//         let mut right = width - 1;
//         while left < right {
//             row.swap(left, right);
//             left += 1;
//             right -= 1;
//         }
//     }
// }



#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    info!("Start Config FRAME_SIZE_BYTES = {}",FRAME_SIZE_BYTES);

    MODE_SETTING.store(0, Ordering::Relaxed);

    let pin = Input::new(p.PIN_22, Pull::Up);
    unwrap!(spawner.spawn(gpio_task(pin)));
    
    let mut led = Output::new(p.PIN_25, Level::Low);

    // LCD display 0: ST7789V pins
    let bl0 = p.PIN_7; // --> BL
    let rst0 = p.PIN_6; // --> RST
    let dcx0 = p.PIN_5; // --> DC
    let cs0 = p.PIN_4; // SPI0 CSN --> CS
    let mosi0 = p.PIN_3; // SPI0 MosiPin --> DIN 
    let sck0 = p.PIN_2; // SPI0 SCK -->  CLK
    let miso0 = p.PIN_20;// SPI0 MisoPin -- unused

    // LCD display 1: ST7789V pins
    let bl1 = p.PIN_14;// --> BL
    let rst1 = p.PIN_13;// --> RST
    let dcx1 = p.PIN_12; // --> DC
    let mosi1 = p.PIN_11; // SPI1 MosiPin --> DIN
    let sck1 = p.PIN_10; // SPI1 SCK --> CLK
    let cs1 = p.PIN_9; // SPI1 CSN --> CS
    let miso1 =  p.PIN_28; // SPI1 MisoPin -- unused

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
    let mut bl0_pwm_out: Pwm<'_> = Pwm::new_output_b(p.PWM_SLICE3, bl0, pwm::Config::default());

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
    // let mut bl1_out = Output::new(bl1, Level::Low);
    let mut bl1_pwm_out: Pwm<'_> = Pwm::new_output_a(p.PWM_SLICE7, bl1, pwm::Config::default());

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
    let inner_eye_fbuf = IRIS_FRAMEBUF.init([0; INNER_EYE_FBUF_SIZE_BYTES]);

    // let eyeframe_left_qoi = Qoi::new(include_bytes!("../img/eye-frame-left-olive.qoi")).unwrap();
    // let eyeframe_left_img: Image<'_, Qoi<'_>> = Image::new(&eyeframe_left_qoi, Point::new(0,0));
    
    let eyeframe_right_qoi = Qoi::new(include_bytes!("../img/eye-frame-right-olive.qoi")).unwrap();
    let eyeframe_right_img: Image<'_, Qoi<'_>> = Image::new(&eyeframe_right_qoi, Point::new(0,0));

    // let left_pupil_ctr: Point = Point::new(320-148,159);
    let right_pupil_ctr: Point = Point::new(148,159);
    let iris_diam = 122;
    let pupil_diam = iris_diam / 2;
    let ideal_iris_ctr = Point::new(iris_diam / 2, iris_diam / 2);

    let highlight_diam = 30;
    // let iris_radius: i32 = iris_diam / 2;
    // let left_iris_tl = Point::new(left_pupil_ctr.x - iris_radius, left_pupil_ctr.y - iris_radius + 25);

    let mut iris_dirty = true ;
    info!("Config done");

    // draw initial background at startup
    redraw_symmetric_bg( &mut left_display, &mut right_display, single_frame_buf, &eyeframe_right_img).await;
  
    // Enable LCD backlight
    // bl0_out.set_high();
    // bl1_out.set_high();

    bl0_pwm_out.set_duty_cycle_percent(10).unwrap();
    bl1_pwm_out.set_duty_cycle_percent(10).unwrap();

    let mut loop_count: usize = 0;
    let iris_colors = [ Rgb565::CSS_BLUE_VIOLET, Rgb565::CSS_DARK_MAGENTA, Rgb565::CSS_YELLOW_GREEN, Rgb565::CSS_MEDIUM_VIOLET_RED, Rgb565::CSS_PALE_VIOLET_RED];
    
    let mut brightness_percent = 10;
    let mut brightness_ascending: bool = true;
    let mut old_mode_val: usize = 555;
    // Main drawing loop, runs forever
    loop {
        led.set_high();
        let mode_val = MODE_SETTING.load(Ordering::Relaxed);

        let iris_color =
            if mode_val == 0 { Rgb565::CSS_MAGENTA }
            else { 
                iris_dirty = true;
                iris_colors[loop_count % iris_colors.len()]
             };

        if old_mode_val != mode_val {
            old_mode_val = mode_val;
            iris_dirty = true;
        }

        // TODO brightness cycling based on mode
        if brightness_ascending {
            brightness_percent += 5;
            if brightness_percent >= 100 {
                brightness_percent = 100;
                brightness_ascending = false;
            }
        }
        else {
            brightness_percent -= 5;
            if brightness_percent <= 10 { 
                brightness_percent = 10;
                brightness_ascending = true; 
            }
        }

        bl0_pwm_out.set_duty_cycle_percent(brightness_percent).unwrap();
        bl1_pwm_out.set_duty_cycle_percent(brightness_percent).unwrap();

        // draw the symmetric inner eye stuff onto right frame, then copy to left
        if iris_dirty {
            let mut raw_fb =
                RawFrameBuf::<Rgb565, _>::new(single_frame_buf.as_mut_slice(), DISPLAY_WIDTH as usize, DISPLAY_HEIGHT as usize);
            draw_symmetric_inner_eye(&mut raw_fb, &right_pupil_ctr, iris_diam, pupil_diam,iris_color).unwrap();
            // while the large frame buf has the right bg stuff in it, copy the small inner fbuf
            let mut inner_buf_dst = inner_eye_fbuf.as_mut_slice();
            copy_centered_region_chunks(
                &single_frame_buf.as_slice(),
                &mut inner_buf_dst, 
                DISPLAY_WIDTH as usize , 
                INNER_EYE_FBUF_W as usize, INNER_EYE_FBUF_H as usize, 
                &right_pupil_ctr);

            right_display
                .show_raw_data(0, 0, 
                    DISPLAY_WIDTH, DISPLAY_HEIGHT, 
                    single_frame_buf)
                .await
                .unwrap();

            // temporarily flip the alternate display orientation so that we can mirror the symmetric background image
            let orig_left_orient = left_display.orientation();
            left_display.set_orientation(orig_left_orient.flip_horizontal()).await.unwrap();
            left_display
                .show_raw_data(0, 0, 
                    DISPLAY_WIDTH, DISPLAY_HEIGHT, 
                    single_frame_buf)
                    .await.unwrap();
            // restore the alternate display orientation
            left_display.set_orientation(orig_left_orient).await.unwrap();
        }

        if iris_dirty {
            let mut inner_eye_rgb565_fbuf  =
                RawFrameBuf::<Rgb565, _>::new(inner_eye_fbuf.as_mut_slice(), INNER_EYE_FBUF_W as usize, INNER_EYE_FBUF_H as usize);
            
            draw_asymmetric_inner_eye(&mut inner_eye_rgb565_fbuf, false, &ideal_iris_ctr, pupil_diam, highlight_diam);

            let dst_frame_x:u16 = (right_pupil_ctr.x - INNER_EYE_FBUF_W as i32/2).try_into().unwrap();
            let dst_frame_y:u16 = (right_pupil_ctr.y - INNER_EYE_FBUF_H as i32/2).try_into().unwrap();
            right_display.show_raw_data(dst_frame_x, dst_frame_y, 
                    INNER_EYE_FBUF_W,  INNER_EYE_FBUF_H, 
                    inner_eye_fbuf)
                .await
                .unwrap();
        }
        // {
        //     // from prior blocks, single_frame_buf contains LEFT display background-- copy inner region to small fbuf
        //     copy_centered_region_chunks(&single_frame_buf.as_slice(),
        //         inner_eye_fbuf.as_mut_slice(), 
        //         DISPLAY_WIDTH as usize , 
        //         INNER_EYE_FBUF_W as usize, INNER_EYE_FBUF_H as usize, 
        //         &left_pupil_ctr);

        //     let mut inner_eye_rgb565_fbuf  =
        //         RawFrameBuf::<Rgb565, _>::new(inner_eye_fbuf.as_mut_slice(), INNER_EYE_FBUF_W as usize, INNER_EYE_FBUF_H as usize);
            
        //     draw_asymmetric_inner_eye(&mut inner_eye_rgb565_fbuf, true, &ideal_iris_ctr, pupil_diam, highlight_diam);
        //     let dst_frame_x:u16 = (left_pupil_ctr.x - INNER_EYE_FBUF_W as i32/2).try_into().unwrap();
        //     let dst_frame_y:u16 = (left_pupil_ctr.y - INNER_EYE_FBUF_H as i32/2).try_into().unwrap();
        //     left_display.show_raw_data( dst_frame_x, dst_frame_y, 
        //             INNER_EYE_FBUF_W,  INNER_EYE_FBUF_H, 
        //             inner_eye_fbuf)
        //         .await
        //         .unwrap();

        // }

        if !iris_dirty {
            Timer::after_millis(250).await;
        }
        iris_dirty = false;

        loop_count += 1;
        led.set_low();
    }
}