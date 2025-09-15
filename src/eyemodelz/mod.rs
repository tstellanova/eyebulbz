use num_enum::TryFromPrimitive;
use heapless::String; // fixed-capacity, no allocator, stack-based
// use heapless::consts::*;
use defmt::Format;

pub const NUM_TWEEN_MORPH_STEPS: u8 = 2; // The number of tween morph steps we support
pub const NUM_LOOK_STEPS: u8 = NUM_TWEEN_MORPH_STEPS + 2; //includes start and end points
pub const NUM_GAZE_SWEEP_STEPS: u8 = (NUM_LOOK_STEPS*2) - 1; // start, middle, end with transitions
pub const SWEEP_MIDDLE_STEP_IDX:u8 = NUM_LOOK_STEPS - 1;
pub const LAST_LOOK_STEP_IDX: u8 = NUM_LOOK_STEPS - 1;


pub fn gaze_and_look_for_sweep_index(sweep_idx: u8) -> (GazeDirection, u8) {
    if sweep_idx > SWEEP_MIDDLE_STEP_IDX {
        (GazeDirection::East, sweep_idx - SWEEP_MIDDLE_STEP_IDX)
    }
    else {
        (GazeDirection::West, SWEEP_MIDDLE_STEP_IDX - sweep_idx)
    }
}

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

