//! Thermistor channel metadata: names, positions, plot colors.
//!
//! Channels 0..6 are probe 1 T1..T6 (the only channels a v1 `Sample`
//! carries); channels 6..12 are probe 2 T1..T6, present only in `Sample12`.
//! Probe-1 ids stay "T1".."T6" so existing capture columns keep their names;
//! probe-2 ids are "P2T1".."P2T6".

use eframe::egui::Color32;

/// Number of thermistor channels across both probes.
pub const CHANNEL_COUNT: usize = 12;
/// Channels per probe (and the channel count of a v1 `Sample`).
pub const PROBE_CHANNELS: usize = 6;

/// Static description of one thermistor channel.
pub struct ChannelInfo {
    /// Short label, e.g. "T1" (probe 1) or "P2T1" (probe 2).
    pub id: &'static str,
    /// Probe number, 1 or 2.
    pub probe: u8,
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

/// Probe 1 T1..T6 then probe 2 T1..T6, in wire order.
pub const CHANNELS: [ChannelInfo; CHANNEL_COUNT] = [
    ChannelInfo {
        id: "T1",
        probe: 1,
        wire_color: "green",
        position: "downstream tip",
        known_broken: true,
        color: Color32::from_rgb(0, 145, 60),
    },
    ChannelInfo {
        id: "T2",
        probe: 1,
        wire_color: "yellow",
        position: "downstream middle",
        known_broken: false,
        color: Color32::from_rgb(190, 145, 0),
    },
    ChannelInfo {
        id: "T3",
        probe: 1,
        wire_color: "purple",
        position: "downstream head",
        known_broken: false,
        color: Color32::from_rgb(140, 60, 200),
    },
    ChannelInfo {
        id: "T4",
        probe: 1,
        wire_color: "orange",
        position: "upstream tip",
        known_broken: false,
        color: Color32::from_rgb(235, 110, 10),
    },
    ChannelInfo {
        id: "T5",
        probe: 1,
        wire_color: "white",
        position: "upstream middle",
        known_broken: false,
        color: Color32::from_rgb(125, 125, 135),
    },
    ChannelInfo {
        id: "T6",
        probe: 1,
        wire_color: "blue",
        position: "upstream head",
        known_broken: false,
        color: Color32::from_rgb(30, 100, 220),
    },
    ChannelInfo {
        id: "P2T1",
        probe: 2,
        wire_color: "green",
        position: "downstream tip",
        known_broken: false,
        color: Color32::from_rgb(0, 90, 40),
    },
    ChannelInfo {
        id: "P2T2",
        probe: 2,
        wire_color: "yellow",
        position: "downstream middle",
        known_broken: false,
        color: Color32::from_rgb(130, 95, 0),
    },
    ChannelInfo {
        id: "P2T3",
        probe: 2,
        wire_color: "purple",
        position: "downstream head",
        known_broken: false,
        color: Color32::from_rgb(90, 30, 140),
    },
    ChannelInfo {
        id: "P2T4",
        probe: 2,
        wire_color: "orange",
        position: "upstream tip",
        known_broken: false,
        color: Color32::from_rgb(165, 70, 0),
    },
    ChannelInfo {
        id: "P2T5",
        probe: 2,
        wire_color: "white",
        position: "upstream middle",
        known_broken: false,
        color: Color32::from_rgb(70, 70, 80),
    },
    ChannelInfo {
        id: "P2T6",
        probe: 2,
        wire_color: "blue",
        position: "upstream head",
        known_broken: false,
        color: Color32::from_rgb(10, 55, 150),
    },
];

/// Color for the excitation trace on raw plots.
pub const EXCITATION_COLOR: Color32 = Color32::from_rgb(70, 70, 70);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_probe_mapping_matches_physical_wire_order() {
        let mapping = CHANNELS
            .each_ref()
            .map(|channel| (channel.wire_color, channel.position, channel.known_broken));
        assert_eq!(
            mapping[..PROBE_CHANNELS],
            [
                ("green", "downstream tip", true),
                ("yellow", "downstream middle", false),
                ("purple", "downstream head", false),
                ("orange", "upstream tip", false),
                ("white", "upstream middle", false),
                ("blue", "upstream head", false),
            ]
        );
        // Probe 2 uses the same wire order; nothing is known broken there.
        for i in 0..PROBE_CHANNELS {
            assert_eq!(mapping[i + PROBE_CHANNELS].0, mapping[i].0);
            assert_eq!(mapping[i + PROBE_CHANNELS].1, mapping[i].1);
            assert!(!mapping[i + PROBE_CHANNELS].2);
            assert_eq!(CHANNELS[i].probe, 1);
            assert_eq!(CHANNELS[i + PROBE_CHANNELS].probe, 2);
        }
        let ids: Vec<_> = CHANNELS.iter().map(|c| c.id).collect();
        assert_eq!(&ids[..PROBE_CHANNELS], ["T1", "T2", "T3", "T4", "T5", "T6"]);
        assert_eq!(ids[PROBE_CHANNELS], "P2T1");
    }
}
