//! The game's frame rate: nothing to read it from here.

use crate::platform::FpsReading;

pub fn game_fps() -> FpsReading {
    FpsReading::Unsupported
}

pub fn stop() {}
