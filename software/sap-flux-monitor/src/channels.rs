//! Thermistor channel metadata: names, positions, plot colors.

use eframe::egui::Color32;

/// Number of thermistor channels.
pub const CHANNEL_COUNT: usize = 6;

/// Static description of one thermistor channel.
pub struct ChannelInfo {
    /// Short label, e.g. "T1".
    pub id: &'static str,
    /// Wire color on the sensor cable.
    pub wire_color: &'static str,
    /// Physical position on the probe.
    pub position: &'static str,
    /// Known broken on the *current* test probe (not a protocol property).
    pub known_broken: bool,
    /// Plot color. Chosen to match the wire color where that stays readable
    /// on a light background (yellow -> amber, white -> gray).
    pub color: Color32,
}

/// T1..T6 in wire order.
pub const CHANNELS: [ChannelInfo; CHANNEL_COUNT] = [
    ChannelInfo {
        id: "T1",
        wire_color: "green",
        position: "downstream tip",
        known_broken: true,
        color: Color32::from_rgb(0, 145, 60),
    },
    ChannelInfo {
        id: "T2",
        wire_color: "yellow",
        position: "downstream middle",
        known_broken: false,
        color: Color32::from_rgb(190, 145, 0),
    },
    ChannelInfo {
        id: "T3",
        wire_color: "purple",
        position: "downstream head",
        known_broken: false,
        color: Color32::from_rgb(140, 60, 200),
    },
    ChannelInfo {
        id: "T4",
        wire_color: "orange",
        position: "upstream tip",
        known_broken: false,
        color: Color32::from_rgb(235, 110, 10),
    },
    ChannelInfo {
        id: "T5",
        wire_color: "white",
        position: "upstream middle",
        known_broken: false,
        color: Color32::from_rgb(125, 125, 135),
    },
    ChannelInfo {
        id: "T6",
        wire_color: "blue",
        position: "upstream head",
        known_broken: false,
        color: Color32::from_rgb(30, 100, 220),
    },
];

/// Color for the excitation trace on raw plots.
pub const EXCITATION_COLOR: Color32 = Color32::from_rgb(70, 70, 70);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_probe_mapping_matches_physical_wire_order() {
        let mapping =
            CHANNELS.map(|channel| (channel.wire_color, channel.position, channel.known_broken));
        assert_eq!(
            mapping,
            [
                ("green", "downstream tip", true),
                ("yellow", "downstream middle", false),
                ("purple", "downstream head", false),
                ("orange", "upstream tip", false),
                ("white", "upstream middle", false),
                ("blue", "upstream head", false),
            ]
        );
    }
}
