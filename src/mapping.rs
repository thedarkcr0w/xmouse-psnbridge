//! From XMouse's controller state (Xbox layout) to the console's (DualShock / DualSense).
//!
//! The buttons sit in the same physical places on both pads, so the mapping is by position:
//! the bottom face button is A on one and Cross on the other, and so on.

use chiaki_core::controller::{
    ControllerState, BUTTON_BOX, BUTTON_CROSS, BUTTON_DPAD_DOWN, BUTTON_DPAD_LEFT,
    BUTTON_DPAD_RIGHT, BUTTON_DPAD_UP, BUTTON_L1, BUTTON_L3, BUTTON_MOON, BUTTON_OPTIONS,
    BUTTON_PS, BUTTON_PYRAMID, BUTTON_R1, BUTTON_R3, BUTTON_SHARE,
};

// XMouse's button bits (its `Action::button_bit`, the Xbox streaming layout).
pub const XBOX_NEXUS: u32 = 1 << 1;
pub const XBOX_MENU: u32 = 1 << 2;
pub const XBOX_VIEW: u32 = 1 << 3;
pub const XBOX_A: u32 = 1 << 4;
pub const XBOX_B: u32 = 1 << 5;
pub const XBOX_X: u32 = 1 << 6;
pub const XBOX_Y: u32 = 1 << 7;
pub const XBOX_DPAD_UP: u32 = 1 << 8;
pub const XBOX_DPAD_DOWN: u32 = 1 << 9;
pub const XBOX_DPAD_LEFT: u32 = 1 << 10;
pub const XBOX_DPAD_RIGHT: u32 = 1 << 11;
pub const XBOX_LB: u32 = 1 << 12;
pub const XBOX_RB: u32 = 1 << 13;
pub const XBOX_LEFT_THUMB: u32 = 1 << 14;
pub const XBOX_RIGHT_THUMB: u32 = 1 << 15;

/// Xbox bit, PlayStation bit, and both names (for the log and the tests). chiaki calls the
/// face buttons by their shapes' nicknames: moon = Circle, box = Square, pyramid = Triangle.
pub const BUTTON_TABLE: [(u32, u32, &str, &str); 15] = [
    (XBOX_A, BUTTON_CROSS, "A", "Cross"),
    (XBOX_B, BUTTON_MOON, "B", "Circle"),
    (XBOX_X, BUTTON_BOX, "X", "Square"),
    (XBOX_Y, BUTTON_PYRAMID, "Y", "Triangle"),
    (XBOX_LB, BUTTON_L1, "LB", "L1"),
    (XBOX_RB, BUTTON_R1, "RB", "R1"),
    (XBOX_LEFT_THUMB, BUTTON_L3, "Left stick click", "L3"),
    (XBOX_RIGHT_THUMB, BUTTON_R3, "Right stick click", "R3"),
    (XBOX_VIEW, BUTTON_SHARE, "View", "Create / Share"),
    (XBOX_MENU, BUTTON_OPTIONS, "Menu", "Options"),
    (XBOX_NEXUS, BUTTON_PS, "Nexus", "PS"),
    (XBOX_DPAD_UP, BUTTON_DPAD_UP, "D-pad up", "D-pad up"),
    (XBOX_DPAD_DOWN, BUTTON_DPAD_DOWN, "D-pad down", "D-pad down"),
    (XBOX_DPAD_LEFT, BUTTON_DPAD_LEFT, "D-pad left", "D-pad left"),
    (XBOX_DPAD_RIGHT, BUTTON_DPAD_RIGHT, "D-pad right", "D-pad right"),
];

pub fn map_buttons(xbox: u32) -> u32 {
    BUTTON_TABLE
        .iter()
        .filter(|(from, ..)| xbox & from != 0)
        .fold(0, |acc, (_, to, ..)| acc | to)
}

/// A stick axis, -1..1, as the console's signed 16-bit value. Both sides use "up is negative",
/// so there is nothing to flip. Anything that isn't a number counts as centred.
pub fn stick(v: f32) -> i16 {
    if !v.is_finite() {
        return 0;
    }
    (v.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

/// A trigger, 0..1, as the console's 0..255. L2 and R2 are analog on the console, like LT/RT.
pub fn trigger(v: f32) -> u8 {
    if !v.is_finite() {
        return 0;
    }
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// One `input` message as the state chiaki sends to the console. The touchpad and the motion
/// sensors have no counterpart in XMouse's message and stay at rest (pad lying flat).
pub fn controller_state(buttons: u32, lx: f32, ly: f32, rx: f32, ry: f32, lt: f32, rt: f32) -> ControllerState {
    ControllerState {
        buttons: map_buttons(buttons),
        l2_state: trigger(lt),
        r2_state: trigger(rt),
        left_x: stick(lx),
        left_y: stick(ly),
        right_x: stick(rx),
        right_y: stick(ry),
        ..ControllerState::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table the task is defined by, with the numbers written out on both sides so a
    /// change to either constant set shows up here.
    #[test]
    fn every_button_goes_to_its_counterpart() {
        let expected: [(&str, u32, &str, u32); 15] = [
            ("A", 16, "Cross", 1 << 0),
            ("B", 32, "Circle", 1 << 1),
            ("X", 64, "Square", 1 << 2),
            ("Y", 128, "Triangle", 1 << 3),
            ("LB", 4096, "L1", 1 << 8),
            ("RB", 8192, "R1", 1 << 9),
            ("Left stick click", 16384, "L3", 1 << 10),
            ("Right stick click", 32768, "R3", 1 << 11),
            ("View", 8, "Create / Share", 1 << 13),
            ("Menu", 4, "Options", 1 << 12),
            ("Nexus", 2, "PS", 1 << 15),
            ("D-pad up", 256, "D-pad up", 1 << 6),
            ("D-pad down", 512, "D-pad down", 1 << 7),
            ("D-pad left", 1024, "D-pad left", 1 << 4),
            ("D-pad right", 2048, "D-pad right", 1 << 5),
        ];
        assert_eq!(BUTTON_TABLE.len(), expected.len());
        for (xbox_name, xbox_bit, ps_name, ps_bit) in expected {
            assert_eq!(map_buttons(xbox_bit), ps_bit, "{xbox_name} should be {ps_name}");
            let row = BUTTON_TABLE.iter().find(|r| r.0 == xbox_bit).expect(xbox_name);
            assert_eq!((row.2, row.3), (xbox_name, ps_name));
        }
    }

    #[test]
    fn combinations_and_unknown_bits() {
        assert_eq!(map_buttons(0), 0);
        assert_eq!(map_buttons(XBOX_A | XBOX_RB | XBOX_DPAD_LEFT), BUTTON_CROSS | BUTTON_R1 | BUTTON_DPAD_LEFT);
        // Bit 0 and everything above bit 15 mean nothing in XMouse's layout: ignored, and in
        // particular never turned into the touchpad click or a trigger.
        assert_eq!(map_buttons(1), 0);
        assert_eq!(map_buttons(0xffff_0000), 0);
        let all = map_buttons(u32::MAX);
        assert_eq!(all.count_ones(), 15);
        assert_eq!(all & (1 << 14), 0, "touchpad click is never pressed");
        // No two Xbox buttons land on the same PlayStation button.
        let mut seen = 0u32;
        for (_, ps, ..) in BUTTON_TABLE {
            assert_eq!(seen & ps, 0);
            seen |= ps;
        }
    }

    #[test]
    fn sticks_and_triggers_cover_the_full_range_and_survive_nonsense() {
        assert_eq!(stick(0.0), 0);
        assert_eq!(stick(1.0), 32767);
        assert_eq!(stick(-1.0), -32767);
        assert_eq!(stick(0.5), 16384);
        assert_eq!(stick(7.0), 32767);
        assert_eq!(stick(-7.0), -32767);
        assert_eq!(trigger(0.0), 0);
        assert_eq!(trigger(1.0), 255);
        assert_eq!(trigger(0.5), 128);
        assert_eq!(trigger(-3.0), 0);
        assert_eq!(trigger(3.0), 255);
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(stick(bad), 0);
            assert_eq!(trigger(bad), 0);
        }
    }

    #[test]
    fn a_whole_input_message_becomes_a_controller_state() {
        let s = controller_state(XBOX_A | XBOX_MENU, 0.5, -1.0, 0.0, 1.0, 0.0, 1.0);
        assert_eq!(s.buttons, BUTTON_CROSS | BUTTON_OPTIONS);
        assert_eq!((s.left_x, s.left_y, s.right_x, s.right_y), (16384, -32767, 0, 32767));
        assert_eq!((s.l2_state, s.r2_state), (0, 255));
        // Everything XMouse doesn't send stays at rest.
        let rest = ControllerState::default();
        assert_eq!(s.touches, rest.touches);
        assert_eq!((s.accel_y, s.orient_w), (rest.accel_y, rest.orient_w));
        // All-zero input is exactly the idle state.
        assert_eq!(controller_state(0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0), rest);
    }
}
