use embedded_graphics::pixelcolor::{Rgb565, RgbColor};
use num_enum::TryFromPrimitive;
use heapless::String; // fixed-capacity, no allocator, stack-based
// use heapless::consts::*;
use defmt::Format;



pub const ORIGINAL_ASSET_GRID_DIM: u8 = 3;
pub const ORIGINAL_ASSET_GRID_AREA: u8 = ORIGINAL_ASSET_GRID_DIM*ORIGINAL_ASSET_GRID_DIM;
pub const NUM_TWEEN_MORPH_STEPS: u8 = 2; // The number of tween morph steps we support
pub const NUM_LOOK_STEPS: u8 = NUM_TWEEN_MORPH_STEPS + 2; //includes start and end points, and transitions
pub const NUM_SHORT_SWEEP_STEPS: u8 = NUM_LOOK_STEPS*2 - 1; // start, middle, end with transitions
pub const SHORT_SWEEP_FLIP_IDX:u8 = (NUM_SHORT_SWEEP_STEPS/2) + 1;

pub const LAST_LOOK_STEP_IDX: u8 = NUM_LOOK_STEPS - 1;

pub const SPARSE_GRID_ARM_LEN: u8 = NUM_LOOK_STEPS - 1;
pub const SPARSE_GRID_DIM: u8 = ORIGINAL_ASSET_GRID_DIM + NUM_TWEEN_MORPH_STEPS*2; // 7 
// pub const SPARSE_GRID_AREA: usize = (SPARSE_GRID_DIM as usize)*(SPARSE_GRID_DIM as usize);
pub const SPARSE_GRID_LEN: u8 = NUM_TWEEN_MORPH_STEPS*(GazeDirection::MaxCount as u8 -1) + ORIGINAL_ASSET_GRID_AREA;
pub const HALF_SPARSE_LEN: u8 = SPARSE_GRID_LEN / 2; // eg for 25 grid, 12
pub const SPARSE_CTR_IDX: u8 = HALF_SPARSE_LEN;












/// Trait for enums that can be converted into a single ASCII digit.
pub trait AsDigit {
    fn as_digit(self) -> u8;
}

// Look direction is a 3x3 grid, with row-col, 00 is northwest, 22 is southeast, 11 is straight ahead
#[derive(Clone, Copy, Debug, Eq, PartialEq, TryFromPrimitive, Format)]
#[repr(u8)]
pub enum EmotionExpression {
    Neutral, // no strong expression
    // Happy = 9,
    Surprise,
    // Curious 
    // Skeptical
    // Thoughtful
    // Confused
    // Shy
    // Love
    // Trepidation 
    MaxCount
}

impl AsDigit for EmotionExpression {
    #[inline]
    fn as_digit(self) -> u8 {
        b'0' + (self as u8)
    }
}

/// A 3x3 grid describing the direction the eyes are looking, 
/// from the observer's perspective.
#[derive(Clone, Copy, Debug, Eq, PartialEq, TryFromPrimitive, Format)]
#[repr(u8)]
pub enum GazeDirection {
    NorthWest = 0,
    North = 1,
    NorthEast = 2,
    West = 3,
    StraightAhead = 4,// straight in front  
    East = 5,
    SouthWest = 6,
    South = 7,
    SouthEast = 8,   
    MaxCount
}

impl GazeDirection {
    // Number of steps including center-> destination -> center
    pub const RT_STEPS_PER_ARM: usize = 7; // 0,1,2,3,2,1,0
    pub const NUM_FULL_SWEEP_STEPS:usize = Self::RT_STEPS_PER_ARM*2;

    pub const CARDINAL_H8_ORDER: [GazeDirection; 8] = [
        GazeDirection::NorthWest,
        GazeDirection::North,
        GazeDirection::NorthEast,
        GazeDirection::West,
        GazeDirection::East,
        GazeDirection::SouthWest,
        GazeDirection::South,
        GazeDirection::SouthEast,
    ];

    pub const CARDINAL_CLOCK_EDGE_ORDER: [GazeDirection; 8] = [
        GazeDirection::NorthWest,
        GazeDirection::North,
        GazeDirection::NorthEast,
        GazeDirection::East,
        GazeDirection::SouthEast,
        GazeDirection::South,
        GazeDirection::SouthWest,
        GazeDirection::West,
    ];

    pub const CARDINAL_ANTICLOCK_EDGE_ORDER: [GazeDirection; 8] = [
        GazeDirection::NorthWest,
        GazeDirection::West,
        GazeDirection::SouthWest,
        GazeDirection::South,
        GazeDirection::SouthEast,
        GazeDirection::East,
        GazeDirection::NorthEast,
        GazeDirection::North,
    ];

    pub const CARDINAL_HSWEEP_ORDER: [GazeDirection; 2] = [
        GazeDirection::West,
        GazeDirection::East,
    ];

    pub const CARDINAL_VSWEEP_ORDER: [GazeDirection; 2] = [
        GazeDirection::North,
        GazeDirection::South,
    ];

    /// Given a monotonically increasing counter, 
    /// and an edge arm traveral order,
    /// return a valid GazeDirection and step index (1..=3) for steps in the gaze direction
    ///  for the sparse 7x7 matrix that represents the asset data we have.
    pub fn gaze_and_step_for_arm_order(counter: usize, arm_order: &[GazeDirection]) -> (GazeDirection, u8) {
        let (dir_raw, offset) = (counter / Self::RT_STEPS_PER_ARM, counter % Self::RT_STEPS_PER_ARM);
        let direction_idx = dir_raw % arm_order.len();

        match offset {
            0 => (GazeDirection::StraightAhead, 0),
            1 => (arm_order[direction_idx], 1),
            2 => (arm_order[direction_idx], 2),
            3 => (arm_order[direction_idx], 3),
            4 => (arm_order[direction_idx], 2),
            5 => (arm_order[direction_idx], 1),
            6 => (GazeDirection::StraightAhead, 0),
            _ => unreachable!(),
        }
    }

    /// Given a raw monotonically increasing counter, sweep through each row in a meandering order
    pub fn gaze_and_look_for_meander(mono_count: usize) -> (GazeDirection, u8) {
        Self::gaze_and_step_for_arm_order(mono_count, &GazeDirection::CARDINAL_H8_ORDER)
    }

    /// Given a monotonically increasing counter, return a valid GazeDirection and
    /// step index for the sparse 7x7 matrix that represents the asset data we have.
    pub fn gaze_and_step_for_sparse_star(mono_count: usize) -> (GazeDirection, u8) {
        Self::gaze_and_step_for_arm_order(mono_count, &GazeDirection::CARDINAL_CLOCK_EDGE_ORDER)
    }

    pub fn gaze_and_step_for_hsweep(mono_count: usize) -> (GazeDirection, u8) {
        Self::gaze_and_step_for_arm_order(mono_count, &GazeDirection::CARDINAL_HSWEEP_ORDER)
    }

    pub fn gaze_and_step_for_vsweep(mono_count: usize) -> (GazeDirection, u8) {
        Self::gaze_and_step_for_arm_order(mono_count, &GazeDirection::CARDINAL_VSWEEP_ORDER)
    }

}

impl GazeDirection {

    /// Provide the (row, column) 3x3 grid index for a gaze direction 
    pub fn row_col(&self) -> (u8, u8) {
        match self {
            GazeDirection::NorthWest => (0,0),
            GazeDirection::North => (0,1),
            GazeDirection::NorthEast => (0,2),
            GazeDirection::West => (1,0),
            GazeDirection::StraightAhead => (1,1),
            GazeDirection::East => (1,2),
            GazeDirection::SouthWest => (2,0),
            GazeDirection::South => (2,1),
            GazeDirection::SouthEast => (2,2),
            GazeDirection::MaxCount => panic!("unsupported row_col"),
        }
    }

    /// Provide the string code for a gaze direction as a 3x3 grid index 
    pub fn to_digits(&self) -> &str {
        match self {
            GazeDirection::NorthWest => "00",
            GazeDirection::North => "01",
            GazeDirection::NorthEast => "02",
            GazeDirection::West => "10",
            GazeDirection::StraightAhead => "11",
            GazeDirection::East => "12",
            GazeDirection::SouthWest => "20",
            GazeDirection::South => "21",
            GazeDirection::SouthEast => "22",
            GazeDirection::MaxCount => panic!("unsupported to_digits"),
        }
    }
}



/// Helper function for generating unique IDs for step-by-step morphed SVG assets.
/// Two formats are supported:
/// - eg "iris_10_0_to_11" or  "iris_10_2_to_11" includes a tween step index
/// - eg "iris_10" is the first asset, "iris_11" is the last, in a 10 -> 11 transition
pub fn stepped_asset_name_full(prefix: &str, start_direction: GazeDirection, end_direction: GazeDirection, look_step_idx: u8) -> String<32>
{
    let mut s: String<32> = String::new();
    s.push_str(prefix).unwrap();
    s.push('_').unwrap();
    if end_direction != start_direction {
        match look_step_idx {
            0 => {
                s.push_str(start_direction.to_digits()).unwrap();
            }
            LAST_LOOK_STEP_IDX => {
                s.push_str(end_direction.to_digits()).unwrap();
            }
            _ => {
                // introduce a tween step tag
                s.push_str(start_direction.to_digits()).unwrap();
                let tween_idx = look_step_idx - 1; //remove start pt
                s.push('_').unwrap();
                s.push(( b'0' + tween_idx) as char).unwrap();
                s.push('_').unwrap();
                s.push_str(end_direction.to_digits()).unwrap();
            }
        }
    }
    else {
        s.push_str(start_direction.to_digits()).unwrap();
    }
    s
}

/// Helper function for generating unique IDs for step-by-step morphed SVG assets.
/// See stepped_asset_name_full for a description of the formats returned.
/// This version assumes a start_direction of  GazeDirection::StraightAhead
pub fn stepped_asset_name(prefix: &str, end_direction: GazeDirection, look_step: u8) -> String<32>
{
    stepped_asset_name_full(prefix, GazeDirection::StraightAhead, end_direction, look_step)
}




// Precomputed fixed-point factors for common lightness adjustments (0.9 * 256, 0.8 * 256, etc.)
pub const FACTOR_DARKEN_10: isize = (0.9 * 256.) as isize; 
pub const FACTOR_DARKEN_20: isize = (0.8 * 256.) as isize;
pub const FACTOR_DARKEN_30: isize = (0.7 * 256.) as isize; 
pub const FACTOR_DARKEN_40: isize = (0.6 * 256.) as isize;
pub const FACTOR_DARKEN_50: isize = (0.5 * 256.) as isize; 
pub const FACTOR_BRIGHTEN_10: isize = (1.1 * 256.) as isize;
pub const FACTOR_BRIGHTEN_20: isize = (1.2 * 256.) as isize;
pub const FACTOR_BRIGHTEN_30: isize = (1.3 * 256.) as isize;
pub const FACTOR_BRIGHTEN_40: isize = (1.4 * 256.) as isize;
pub const FACTOR_BRIGHTEN_50: isize = (1.5 * 256.) as isize;

/// Adjust lightness of Rgb565 color by a fixed-point factor
/// factor: 256 = no change, <256 = darker, >256 = brighter
pub fn adjust_lightness_rgb565(color: Rgb565, factor: isize) -> Rgb565 {
    // Extract RGB components (they're already u8 in Rgb565)]
    let r = color.r() as isize;
    let g = color.g() as isize;
    let b = color.b() as isize;
    
    // Scale components using fixed-point arithmetic
    let scaled_r = (r * factor) >> 8;
    let scaled_g = (g * factor) >> 8;
    let scaled_b = (b * factor) >> 8;
    
    // // Clamp to valid u8 ranges (Rgb565 stores as u8 internally)
    let r_final = scaled_r.clamp(0, 0x1F) as u8;
    let g_final = scaled_g.clamp(0, 0x3F) as u8;
    let b_final = scaled_b.clamp(0, 0x1F) as u8;
    
    // Create new Rgb565 color
    Rgb565::new(r_final, g_final, b_final)
}