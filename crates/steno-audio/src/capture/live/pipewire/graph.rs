//! What the PipeWire backend knows about the graph: the nodes and ports the
//! registry announced and the two defaults from the `default` metadata,
//! and the decisions made from them: which ports feed which channel of
//! Steno's capture stream, the [`DeviceSnapshot`] a change is judged by,
//! and a port's latency in frames. Pure, so the choices are unit-tested
//! without a PipeWire daemon; `super` feeds it from the registry and
//! metadata callbacks.
//!
//! Identities: a device's UID (what `Settings.input_device_uid` stores) is
//! its `node.name`, stable across reboots for the same hardware, as the
//! `default.audio.*` metadata names devices too. The microphone is the
//! first non-monitor output port (lowest `port.id`) of the input node, as
//! the macOS backend takes the input device's first channel; a virtual
//! source (a null sink with `media.class = Audio/Source/Virtual`) has only
//! a monitor output, and that is taken then. The system
//! lane is the default sink's monitor: its `FL` and `FR` monitor ports
//! (folded to mono by the rings), or its only one for a mono sink, or the
//! two lowest-numbered for a sink without front channels.

use std::collections::BTreeMap;

use steno_core::AudioLane;

use crate::SAMPLE_RATE;
use crate::capture::{CaptureError, ChannelRef, DeviceSnapshot, LaneSource, StreamLayout};

/// The metadata key WirePlumber keeps the default sink's name in.
pub(crate) const DEFAULT_SINK_KEY: &str = "default.audio.sink";
/// The metadata key WirePlumber keeps the default source's name in.
pub(crate) const DEFAULT_SOURCE_KEY: &str = "default.audio.source";

/// A node the registry announced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NodeEntry {
    /// `node.name`: the UID.
    pub name: String,
    /// `media.class`, `Audio/Sink`, `Audio/Source` and so on.
    pub media_class: String,
}

impl NodeEntry {
    /// Something a microphone lane can record from.
    fn is_source(&self) -> bool {
        self.media_class.starts_with("Audio/Source") || self.media_class == "Audio/Duplex"
    }

    /// Something whose monitor the system lane can record.
    fn is_sink(&self) -> bool {
        self.media_class.starts_with("Audio/Sink") || self.media_class == "Audio/Duplex"
    }
}

/// A port the registry announced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PortEntry {
    /// The owning node's global id.
    pub node: u32,
    /// `port.direction` is `out`.
    pub output: bool,
    /// `port.monitor` is `true`: a sink's copy of what it plays.
    pub monitor: bool,
    /// `audio.channel`: `FL`, `MONO`, `AUX0` and so on.
    pub channel: String,
    /// `port.id`: the port's index on its node.
    pub index: u32,
}

/// One end the capture stream is linked to: the node and the ports taken
/// from it, in channel order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Endpoint {
    /// The node's global id.
    pub node: u32,
    /// Its `node.name`.
    pub name: String,
    /// The ports linked from it, in stream channel order.
    pub ports: Vec<u32>,
    /// The port whose latency is the device path's: the microphone port
    /// itself, or the sink's first playback (input) port.
    pub latency_port: Option<u32>,
}

/// What a capture links and how the lanes sit in its interleaved buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Targets {
    /// The input device, for a `Mic` or `Mixed` lane.
    pub mic: Option<Endpoint>,
    /// The default sink, for a `System` lane.
    pub output: Option<Endpoint>,
    /// `(node, port)` feeding each channel of the stream, in channel order.
    pub feeds: Vec<(u32, u32)>,
    /// One interleaved buffer of `feeds.len()` channels.
    pub layout: StreamLayout,
}

impl Targets {
    /// Channels in the capture stream's format.
    pub fn channels(&self) -> usize {
        self.feeds.len()
    }
}

/// The registry and the defaults as last announced; see the module doc.
#[derive(Debug, Default, Clone)]
pub(crate) struct Graph {
    nodes: BTreeMap<u32, NodeEntry>,
    ports: BTreeMap<u32, PortEntry>,
    default_sink: Option<String>,
    default_source: Option<String>,
}

impl Graph {
    /// A node global, its properties read through `props`. Nodes without a
    /// `node.name` are not kept: nothing can name them.
    pub fn add_node<'a>(&mut self, id: u32, props: impl Fn(&str) -> Option<&'a str>) {
        let Some(name) = props("node.name") else {
            return;
        };
        self.nodes.insert(
            id,
            NodeEntry {
                name: name.to_owned(),
                media_class: props("media.class").unwrap_or_default().to_owned(),
            },
        );
    }

    /// A port global, its properties read through `props`. Ports without
    /// an owning node or a direction are not kept.
    pub fn add_port<'a>(&mut self, id: u32, props: impl Fn(&str) -> Option<&'a str>) {
        let Some(node) = props("node.id").and_then(|v| v.parse().ok()) else {
            return;
        };
        let output = match props("port.direction") {
            Some("out") => true,
            Some("in") => false,
            _ => return,
        };
        self.ports.insert(
            id,
            PortEntry {
                node,
                output,
                monitor: props("port.monitor") == Some("true"),
                channel: props("audio.channel").unwrap_or_default().to_owned(),
                index: props("port.id")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(u32::MAX),
            },
        );
    }

    /// A global went away. True when it was a node: the change a capture
    /// may have to react to (a port goes with its node).
    pub fn remove(&mut self, id: u32) -> bool {
        self.ports.remove(&id);
        self.nodes.remove(&id).is_some()
    }

    /// A property of the `default` metadata on subject 0. `key` `None`
    /// clears every key; `value` `None` removes one. The value is JSON,
    /// `{"name":"alsa_output…"}`. True when a default this module reads
    /// changed.
    pub fn set_default(&mut self, key: Option<&str>, value: Option<&str>) -> bool {
        let (sink, source) = match key {
            None => (true, true),
            Some(DEFAULT_SINK_KEY) => (true, false),
            Some(DEFAULT_SOURCE_KEY) => (false, true),
            Some(_) => return false,
        };
        let name = value.and_then(default_name);
        let mut changed = false;
        if sink && self.default_sink != name {
            self.default_sink.clone_from(&name);
            changed = true;
        }
        if source && self.default_source != name {
            self.default_source = name;
            changed = true;
        }
        changed
    }

    /// Whether `endpoint`'s node is still in the graph: its id still names
    /// a node of its name (PipeWire reuses the ids of removed globals).
    fn is_alive(&self, endpoint: &Endpoint) -> bool {
        self.nodes
            .get(&endpoint.node)
            .is_some_and(|node| node.name == endpoint.name)
    }

    /// The source a microphone lane records: the node named `uid`, or the
    /// default source.
    fn source_named(&self, uid: Option<&str>) -> Option<(u32, &NodeEntry)> {
        let name = uid.or(self.default_source.as_deref())?;
        self.nodes
            .iter()
            .find(|(_, node)| node.name == name && node.is_source())
            .map(|(id, node)| (*id, node))
    }

    /// The default sink.
    fn default_sink_node(&self) -> Option<(u32, &NodeEntry)> {
        let name = self.default_sink.as_deref()?;
        self.nodes
            .iter()
            .find(|(_, node)| node.name == name && node.is_sink())
            .map(|(id, node)| (*id, node))
    }

    /// Node `node`'s ports matching `keep`, by `port.id`.
    fn ports_of(&self, node: u32, keep: impl Fn(&PortEntry) -> bool) -> Vec<(u32, &PortEntry)> {
        let mut ports: Vec<(u32, &PortEntry)> = self
            .ports
            .iter()
            .filter(|(_, port)| port.node == node && keep(port))
            .map(|(id, port)| (*id, port))
            .collect();
        ports.sort_by_key(|(id, port)| (port.index, *id));
        ports
    }

    /// The microphone endpoint for `uid` (the default source when `None`).
    fn mic(&self, uid: Option<&str>) -> Result<Endpoint, CaptureError> {
        let (node, entry) = self
            .source_named(uid)
            .ok_or(CaptureError::InputDeviceUnavailable)?;
        let outputs = self.ports_of(node, |p| p.output);
        let port = outputs
            .iter()
            .find(|(_, p)| !p.monitor)
            .or_else(|| outputs.first())
            .map(|(id, _)| *id)
            .ok_or(CaptureError::InputDeviceUnavailable)?;
        Ok(Endpoint {
            node,
            name: entry.name.clone(),
            ports: vec![port],
            latency_port: Some(port),
        })
    }

    /// The default sink's monitor endpoint: front left and right first,
    /// at most two ports.
    fn output(&self) -> Result<Endpoint, CaptureError> {
        let (node, entry) = self
            .default_sink_node()
            .ok_or(CaptureError::OutputDeviceUnavailable)?;
        let mut monitors = self.ports_of(node, |p| p.output && p.monitor);
        let rank = |port: &PortEntry| match port.channel.as_str() {
            "FL" => 0,
            "FR" => 1,
            _ => 2,
        };
        // Stable: equal ranks keep their `port.id` order.
        monitors.sort_by_key(|(_, port)| rank(port));
        if monitors.is_empty() {
            return Err(CaptureError::UnexpectedStreamLayout(format!(
                "the output {} has no monitor ports",
                entry.name
            )));
        }
        let latency_port = self
            .ports_of(node, |p| !p.output)
            .first()
            .map(|(id, _)| *id);
        Ok(Endpoint {
            node,
            name: entry.name.clone(),
            ports: monitors.iter().take(2).map(|(id, _)| *id).collect(),
            latency_port,
        })
    }

    /// What a capture of `lanes` links: the endpoints, the port feeding
    /// each stream channel, and the layout of the interleaved buffer.
    /// `InputDeviceUnavailable` or `OutputDeviceUnavailable` when a lane's
    /// device does not resolve.
    pub fn resolve(&self, lanes: &[AudioLane], uid: Option<&str>) -> Result<Targets, CaptureError> {
        if lanes.is_empty() {
            return Err(CaptureError::UnexpectedStreamLayout(
                "a capture needs at least one lane".into(),
            ));
        }
        let needs_mic = lanes
            .iter()
            .any(|l| matches!(l, AudioLane::Mic | AudioLane::Mixed));
        let mic = needs_mic.then(|| self.mic(uid)).transpose()?;
        let output = lanes
            .contains(&AudioLane::System)
            .then(|| self.output())
            .transpose()?;

        let mut feeds = Vec::new();
        let mut channels = Vec::with_capacity(lanes.len());
        for lane in lanes {
            let endpoint = match lane {
                AudioLane::Mic | AudioLane::Mixed => mic.as_ref(),
                AudioLane::System => output.as_ref(),
            }
            .expect("resolved above for every lane present");
            channels.push((*lane, feeds.len(), endpoint.ports.len()));
            feeds.extend(endpoint.ports.iter().map(|port| (endpoint.node, *port)));
        }
        let stride = feeds.len();
        let sources = channels
            .into_iter()
            .map(|(lane, first, count)| LaneSource {
                lane,
                left: ChannelRef::new(0, first, stride),
                right: (count >= 2).then(|| ChannelRef::new(0, first + 1, stride)),
            })
            .collect();
        Ok(Targets {
            mic,
            output,
            feeds,
            layout: StreamLayout {
                sources,
                tap_first: false,
            },
        })
    }

    /// The capture stream's input ports on node `stream`, one per channel
    /// in channel order (`AUX0`, `AUX1`, …), once all `channels` exist.
    pub fn stream_ports(&self, stream: u32, channels: usize) -> Option<Vec<u32>> {
        let inputs = self.ports_of(stream, |p| !p.output);
        (0..channels)
            .map(|channel| {
                let name = format!("AUX{channel}");
                inputs
                    .iter()
                    .find(|(_, port)| port.channel == name)
                    .map(|(id, _)| *id)
            })
            .collect()
    }

    /// The devices as they are now, for a capture that resolved `targets`
    /// with `uid`; `lost` once the connection or the stream failed. The
    /// default sink stands for both of the snapshot's outputs (PipeWire has
    /// no separate clock master: the graph resamples to the stream's 48
    /// kHz), so `default_output_uid` stays `None` and `sample_rate` stays
    /// [`SAMPLE_RATE`].
    pub fn snapshot(&self, targets: &Targets, uid: Option<&str>, lost: bool) -> DeviceSnapshot {
        let alive = |endpoint: &Option<Endpoint>| {
            !lost && endpoint.as_ref().is_some_and(|e| self.is_alive(e))
        };
        DeviceSnapshot {
            output_uid: targets
                .output
                .as_ref()
                .and_then(|_| self.default_sink_node())
                .map(|(_, node)| node.name.clone()),
            default_output_uid: None,
            input_uid: targets
                .mic
                .as_ref()
                .and_then(|_| self.source_named(uid))
                .map(|(_, node)| node.name.clone()),
            output_alive: alive(&targets.output),
            input_alive: alive(&targets.mic),
            sample_rate: SAMPLE_RATE,
        }
    }
}

/// The `name` of a `default` metadata value, `{"name":"…"}`.
fn default_name(value: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(value).ok()?;
    json.get("name")?.as_str().map(str::to_owned)
}

/// The lower bound of a port's `SPA_PARAM_Latency` in one direction:
/// graph quanta, samples at the graph rate and nanoseconds, summed.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct Latency {
    /// Multiples of the graph's quantum.
    pub quantum: f32,
    /// Samples at the graph rate.
    pub rate: i32,
    /// Nanoseconds.
    pub ns: i64,
}

impl Latency {
    /// The latency in frames at [`SAMPLE_RATE`]. `cycle_frames` is one
    /// graph cycle as the capture stream receives it (the quantum,
    /// resampled to 48 kHz), `graph_rate` the graph's clock in hertz. The
    /// lower bounds are used and the sum is rounded down: the far-end delay
    /// must not overshoot (see `CaptureSession::far_end_delay_frames`).
    pub fn frames(&self, cycle_frames: usize, graph_rate: u32) -> usize {
        let graph_rate = if graph_rate == 0 {
            SAMPLE_RATE
        } else {
            f64::from(graph_rate)
        };
        let frames = f64::from(self.quantum) * cycle_frames as f64
            + f64::from(self.rate) * SAMPLE_RATE / graph_rate
            + self.ns as f64 * SAMPLE_RATE / 1e9;
        if frames.is_finite() && frames > 0.0 {
            frames.floor() as usize
        } else {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<&'a str> {
        move |key| pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }

    /// A laptop: a stereo ALSA sink with monitors, a stereo built-in
    /// microphone, a mono USB headset microphone, the defaults set.
    fn laptop() -> Graph {
        let mut graph = Graph::default();
        graph.add_node(
            40,
            props(&[
                ("node.name", "alsa_output.pci.analog-stereo"),
                ("media.class", "Audio/Sink"),
            ]),
        );
        graph.add_node(
            41,
            props(&[
                ("node.name", "alsa_input.pci.analog-stereo"),
                ("media.class", "Audio/Source"),
            ]),
        );
        graph.add_node(
            42,
            props(&[
                ("node.name", "alsa_input.usb-headset.mono"),
                ("media.class", "Audio/Source"),
            ]),
        );
        let port = |graph: &mut Graph, id: u32, node: &str, dir: &str, ch: &str, index: &str| {
            let monitor = if ch.starts_with("monitor_") {
                "true"
            } else {
                "false"
            };
            let channel = ch.trim_start_matches("monitor_");
            graph.add_port(
                id,
                props(&[
                    ("node.id", node),
                    ("port.direction", dir),
                    ("port.monitor", monitor),
                    ("audio.channel", channel),
                    ("port.id", index),
                ]),
            );
        };
        port(&mut graph, 50, "40", "in", "FL", "0");
        port(&mut graph, 51, "40", "in", "FR", "1");
        // The monitor ports announced right before left, as the registry
        // may.
        port(&mut graph, 52, "40", "out", "monitor_FR", "1");
        port(&mut graph, 53, "40", "out", "monitor_FL", "0");
        port(&mut graph, 54, "41", "out", "FR", "1");
        port(&mut graph, 55, "41", "out", "FL", "0");
        port(&mut graph, 56, "42", "out", "MONO", "0");
        graph.set_default(
            Some(DEFAULT_SINK_KEY),
            Some(r#"{"name":"alsa_output.pci.analog-stereo"}"#),
        );
        graph.set_default(
            Some(DEFAULT_SOURCE_KEY),
            Some(r#"{"name":"alsa_input.pci.analog-stereo"}"#),
        );
        graph
    }

    #[test]
    fn a_call_links_the_first_mic_channel_and_both_front_monitors() {
        let graph = laptop();
        let targets = graph
            .resolve(&[AudioLane::Mic, AudioLane::System], None)
            .unwrap();
        assert_eq!(targets.feeds, vec![(41, 55), (40, 53), (40, 52)]);
        assert_eq!(targets.channels(), 3);
        assert_eq!(
            targets.layout.sources,
            vec![
                LaneSource {
                    lane: AudioLane::Mic,
                    left: ChannelRef::new(0, 0, 3),
                    right: None,
                },
                LaneSource {
                    lane: AudioLane::System,
                    left: ChannelRef::new(0, 1, 3),
                    right: Some(ChannelRef::new(0, 2, 3)),
                },
            ]
        );
        assert_eq!(targets.mic.unwrap().latency_port, Some(55));
        assert_eq!(targets.output.unwrap().latency_port, Some(50));
    }

    #[test]
    fn an_explicit_uid_wins_over_the_default_source() {
        let targets = laptop()
            .resolve(&[AudioLane::Mixed], Some("alsa_input.usb-headset.mono"))
            .unwrap();
        assert_eq!(targets.feeds, vec![(42, 56)]);
        assert_eq!(targets.output, None);
        assert_eq!(
            targets.layout.sources[0].left,
            ChannelRef::new(0, 0, 1),
            "in person is one mono channel"
        );
    }

    #[test]
    fn a_virtual_source_records_from_its_monitor_output() {
        let mut graph = laptop();
        graph.add_node(
            60,
            props(&[
                ("node.name", "virtual-mic"),
                ("media.class", "Audio/Source/Virtual"),
            ]),
        );
        for (id, dir, monitor) in [(61, "in", "false"), (62, "out", "true")] {
            graph.add_port(
                id,
                props(&[
                    ("node.id", "60"),
                    ("port.direction", dir),
                    ("port.monitor", monitor),
                    ("audio.channel", "MONO"),
                    ("port.id", "0"),
                ]),
            );
        }
        let targets = graph
            .resolve(&[AudioLane::Mic], Some("virtual-mic"))
            .unwrap();
        assert_eq!(targets.feeds, vec![(60, 62)]);
    }

    #[test]
    fn a_system_only_capture_needs_no_microphone() {
        let mut graph = laptop();
        graph.set_default(Some(DEFAULT_SOURCE_KEY), None);
        let targets = graph.resolve(&[AudioLane::System], None).unwrap();
        assert_eq!(targets.feeds, vec![(40, 53), (40, 52)]);
        assert_eq!(targets.mic, None);
    }

    #[test]
    fn missing_devices_fail_with_the_lane_they_belong_to() {
        let graph = laptop();
        assert_eq!(
            graph.resolve(&[AudioLane::Mic], Some("gone")),
            Err(CaptureError::InputDeviceUnavailable)
        );
        assert_eq!(
            graph.resolve(&[AudioLane::Mic], Some("alsa_output.pci.analog-stereo")),
            Err(CaptureError::InputDeviceUnavailable),
            "a sink is not a microphone"
        );
        let mut no_sink = laptop();
        no_sink.set_default(Some(DEFAULT_SINK_KEY), None);
        assert_eq!(
            no_sink.resolve(&[AudioLane::Mic, AudioLane::System], None),
            Err(CaptureError::OutputDeviceUnavailable)
        );
        assert!(matches!(
            graph.resolve(&[], None),
            Err(CaptureError::UnexpectedStreamLayout(_))
        ));
    }

    #[test]
    fn a_mono_sink_feeds_one_channel_and_a_sink_without_monitors_fails() {
        let mut graph = Graph::default();
        graph.add_node(
            1,
            props(&[("node.name", "mono-sink"), ("media.class", "Audio/Sink")]),
        );
        graph.set_default(Some(DEFAULT_SINK_KEY), Some(r#"{"name":"mono-sink"}"#));
        assert!(matches!(
            graph.resolve(&[AudioLane::System], None),
            Err(CaptureError::UnexpectedStreamLayout(_))
        ));
        graph.add_port(
            2,
            props(&[
                ("node.id", "1"),
                ("port.direction", "out"),
                ("port.monitor", "true"),
                ("audio.channel", "MONO"),
                ("port.id", "0"),
            ]),
        );
        let targets = graph.resolve(&[AudioLane::System], None).unwrap();
        assert_eq!(targets.feeds, vec![(1, 2)]);
        assert_eq!(targets.layout.sources[0].right, None);
        assert_eq!(targets.output.unwrap().latency_port, None);
    }

    #[test]
    fn stream_ports_wait_for_every_channel() {
        let mut graph = laptop();
        let aux = |graph: &mut Graph, id: u32, channel: &str| {
            graph.add_port(
                id,
                props(&[
                    ("node.id", "90"),
                    ("port.direction", "in"),
                    ("audio.channel", channel),
                ]),
            );
        };
        aux(&mut graph, 92, "AUX1");
        assert_eq!(graph.stream_ports(90, 2), None);
        aux(&mut graph, 91, "AUX0");
        assert_eq!(graph.stream_ports(90, 2), Some(vec![91, 92]));
    }

    #[test]
    fn defaults_parse_change_and_clear() {
        let mut graph = laptop();
        assert!(!graph.set_default(
            Some(DEFAULT_SINK_KEY),
            Some(r#"{ "name": "alsa_output.pci.analog-stereo" }"#)
        ));
        assert!(!graph.set_default(Some("default.video.source"), Some("{}")));
        assert!(graph.set_default(Some(DEFAULT_SINK_KEY), Some("not json")));
        assert_eq!(
            graph.resolve(&[AudioLane::System], None),
            Err(CaptureError::OutputDeviceUnavailable)
        );
        assert!(graph.set_default(None, None));
        assert_eq!(
            graph.resolve(&[AudioLane::Mic], None),
            Err(CaptureError::InputDeviceUnavailable)
        );
    }

    #[test]
    fn the_snapshot_sees_defaults_move_and_devices_go() {
        let mut graph = laptop();
        let lanes = [AudioLane::Mic, AudioLane::System];
        let targets = graph.resolve(&lanes, None).unwrap();
        let baseline = graph.snapshot(&targets, None, false);
        assert_eq!(
            baseline.output_uid.as_deref(),
            Some("alsa_output.pci.analog-stereo")
        );
        assert_eq!(
            baseline.input_uid.as_deref(),
            Some("alsa_input.pci.analog-stereo")
        );
        assert!(baseline.output_alive && baseline.input_alive);
        assert_eq!(
            graph.snapshot(&targets, None, false).difference(&baseline),
            None
        );

        graph.set_default(
            Some(DEFAULT_SOURCE_KEY),
            Some(r#"{"name":"alsa_input.usb-headset.mono"}"#),
        );
        assert_eq!(
            graph.snapshot(&targets, None, false).difference(&baseline),
            Some(crate::capture::DeviceChangeReason::DefaultInputChanged)
        );
        assert_eq!(
            graph.snapshot(&targets, None, true).difference(&baseline),
            Some(crate::capture::DeviceChangeReason::OutputDeviceGone),
            "a lost connection loses the output first"
        );
        assert!(graph.remove(40));
        assert!(!graph.remove(55), "a port is not a node");
        graph.add_node(
            40,
            props(&[("node.name", "a-new-node"), ("media.class", "Audio/Sink")]),
        );
        assert_eq!(
            graph.snapshot(&targets, None, false).difference(&baseline),
            Some(crate::capture::DeviceChangeReason::OutputDeviceGone),
            "a reused id is not the device that went"
        );
    }

    #[test]
    fn an_explicit_microphone_ignores_the_default_and_reports_its_loss() {
        let mut graph = laptop();
        let uid = Some("alsa_input.usb-headset.mono");
        let targets = graph.resolve(&[AudioLane::Mixed], uid).unwrap();
        let baseline = graph.snapshot(&targets, uid, false);
        assert_eq!(baseline.output_uid, None, "in person watches no output");
        graph.set_default(Some(DEFAULT_SOURCE_KEY), None);
        graph.set_default(Some(DEFAULT_SINK_KEY), None);
        assert_eq!(
            graph.snapshot(&targets, uid, false).difference(&baseline),
            None
        );
        graph.remove(42);
        assert_eq!(
            graph.snapshot(&targets, uid, false).difference(&baseline),
            Some(crate::capture::DeviceChangeReason::InputDeviceGone)
        );
    }

    #[test]
    fn latency_sums_quanta_samples_and_nanoseconds_at_48_khz() {
        let alsa = Latency {
            quantum: 1.0,
            rate: 256,
            ns: 0,
        };
        assert_eq!(alsa.frames(1_024, 48_000), 1_280);
        // A 44.1 kHz graph: the cycle arrives resampled, the samples scale.
        assert_eq!(alsa.frames(1_114, 44_100), 1_114 + 278);
        let bluetooth = Latency {
            quantum: 0.0,
            rate: 0,
            ns: 40_000_000,
        };
        assert_eq!(bluetooth.frames(1_024, 0), 1_920);
        assert_eq!(Latency::default().frames(1_024, 48_000), 0);
        let negative = Latency {
            quantum: 0.0,
            rate: -10,
            ns: 0,
        };
        assert_eq!(negative.frames(1_024, 48_000), 0);
    }
}
