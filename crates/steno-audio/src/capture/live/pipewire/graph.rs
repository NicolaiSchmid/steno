//! What the PipeWire backend knows about the graph (the registry's nodes
//! and ports and the `default` metadata) and the decisions made from it:
//! which ports feed which channel, which lane each link serves, and the
//! [`DeviceSnapshot`] a change is judged by. A device's UID is its
//! `node.name`; a node or port is alive while its id keeps its serial.

use std::collections::BTreeMap;

use steno_core::AudioLane;

use crate::SAMPLE_RATE;
use crate::capture::live::AudioDeviceInfo;
use crate::capture::{
    CaptureError, CaptureInput, ChannelRef, DeviceSnapshot, LaneSource, StreamLayout,
};

/// The metadata key WirePlumber keeps the default sink's name in.
pub(crate) const DEFAULT_SINK_KEY: &str = "default.audio.sink";
/// The metadata key WirePlumber keeps the default source's name in.
pub(crate) const DEFAULT_SOURCE_KEY: &str = "default.audio.source";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NodeEntry {
    pub name: String,
    pub media_class: String,
    /// `node.description`, else `node.nick`: the name a person reads.
    pub description: Option<String>,
    /// `object.serial`, unique for the daemon's lifetime.
    pub serial: Option<u64>,
    /// A Bluetooth node (`device.api` `bluez5`): linking a stream to its
    /// input switches the headset's profile, under the output too.
    pub bluetooth: bool,
}

/// Whether a node of `media_class` is something a microphone lane can
/// record from: a source (a virtual one included) or a duplex device.
pub(crate) fn is_source_class(media_class: &str) -> bool {
    media_class.starts_with("Audio/Source") || media_class == "Audio/Duplex"
}

impl NodeEntry {
    fn is_source(&self) -> bool {
        is_source_class(&self.media_class)
    }

    fn is_sink(&self) -> bool {
        self.media_class.starts_with("Audio/Sink") || self.media_class == "Audio/Duplex"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PortEntry {
    pub node: u32,
    pub output: bool,
    /// `port.monitor` is `true`: a sink's copy of what it plays.
    pub monitor: bool,
    pub channel: String,
    /// `port.id`: the port's index on its node.
    pub index: u32,
    /// `object.serial`, unique for the daemon's lifetime.
    pub serial: Option<u64>,
}

/// One end the capture stream is linked to: the node and its ports, in
/// channel order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Endpoint {
    pub node: u32,
    pub name: String,
    pub ports: Vec<u32>,
    /// The port whose latency is the device path's: the microphone port
    /// itself, or the sink's first playback (input) port.
    pub latency_port: Option<u32>,
    /// The `object.serial` of the node, then of each of `ports`.
    serials: Vec<Option<u64>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Targets {
    /// The input device, for a `Mic` or `Mixed` lane.
    pub mic: Option<Endpoint>,
    /// The default sink, for a `System` lane.
    pub output: Option<Endpoint>,
    /// `(node, port)` feeding each channel of the stream, in channel order.
    pub feeds: Vec<(u32, u32)>,
    pub layout: StreamLayout,
}

impl Targets {
    pub fn channels(&self) -> usize {
        self.feeds.len()
    }

    /// What losing Steno's link from `port` of `node` loses: the lane whose
    /// endpoint takes that port; both for a port neither takes.
    pub fn lost_with(&self, node: u32, port: u32) -> Lost {
        let takes = |endpoint: &Option<Endpoint>| {
            endpoint
                .as_ref()
                .is_some_and(|e| e.node == node && e.ports.contains(&port))
        };
        let lost = Lost {
            mic: takes(&self.mic),
            output: takes(&self.output),
        };
        if lost.any() { lost } else { Lost::ALL }
    }
}

/// What a capture lost of its devices: Steno's links from the microphone,
/// the output, or both.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Lost {
    /// Steno's link from the microphone failed or went.
    pub mic: bool,
    /// One of Steno's links from the output failed or went.
    pub output: bool,
}

impl Lost {
    pub const NONE: Self = Self {
        mic: false,
        output: false,
    };

    /// What a lost connection or stream loses.
    pub const ALL: Self = Self {
        mic: true,
        output: true,
    };

    pub fn any(self) -> bool {
        self.mic || self.output
    }

    #[must_use]
    pub fn union(self, other: Self) -> Self {
        Self {
            mic: self.mic || other.mic,
            output: self.output || other.output,
        }
    }
}

/// The registry and the defaults as last announced.
#[derive(Debug, Default, Clone)]
pub(crate) struct Graph {
    nodes: BTreeMap<u32, NodeEntry>,
    ports: BTreeMap<u32, PortEntry>,
    default_sink: Option<String>,
    default_source: Option<String>,
    /// The targets and the UID asked for by the capture the graph serves.
    tracked: Option<(Targets, Option<String>)>,
}

impl Graph {
    /// A node global. Nodes without a `node.name` are not kept. True when it
    /// is the node the tracked capture asked for.
    pub fn add_node<'a>(&mut self, id: u32, props: impl Fn(&str) -> Option<&'a str>) -> bool {
        let Some(name) = props("node.name") else {
            return false;
        };
        let chosen = self.is_chosen(name);
        self.nodes.insert(
            id,
            NodeEntry {
                name: name.to_owned(),
                media_class: props("media.class").unwrap_or_default().to_owned(),
                description: props("node.description")
                    .or_else(|| props("node.nick"))
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned),
                serial: serial(&props),
                bluetooth: props("device.api") == Some("bluez5"),
            },
        );
        chosen
    }

    /// A port global. Ports without a node or a direction are not kept. True
    /// when it is a port of the node the tracked capture asked for.
    pub fn add_port<'a>(&mut self, id: u32, props: impl Fn(&str) -> Option<&'a str>) -> bool {
        let Some(node) = props("node.id").and_then(|v| v.parse().ok()) else {
            return false;
        };
        let output = match props("port.direction") {
            Some("out") => true,
            Some("in") => false,
            _ => return false,
        };
        let chosen = self
            .nodes
            .get(&node)
            .is_some_and(|entry| self.is_chosen(&entry.name));
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
                serial: serial(&props),
            },
        );
        chosen
    }

    fn is_chosen(&self, name: &str) -> bool {
        self.tracked
            .as_ref()
            .is_some_and(|(_, uid)| uid.as_deref() == Some(name))
    }

    /// From now on a removal counts as a change only when the snapshot for
    /// `targets` with `uid` reads what went.
    pub fn track(&mut self, targets: &Targets, uid: Option<&str>) {
        self.tracked = Some((targets.clone(), uid.map(str::to_owned)));
    }

    /// A global went away. True when the capture may have to react: before
    /// [`Self::track`] any node or port, after it a node the snapshot reads
    /// or a port of one, so another app's stream ending is no change.
    pub fn remove(&mut self, id: u32) -> bool {
        let node = match self.ports.get(&id) {
            Some(port) => Some(port.node),
            None => self.nodes.contains_key(&id).then_some(id),
        };
        let changed = node.is_some_and(|node| {
            self.tracked
                .as_ref()
                .is_none_or(|(targets, uid)| self.is_read(node, targets, uid.as_deref()))
        });
        self.ports.remove(&id);
        self.nodes.remove(&id);
        changed
    }

    /// Whether the snapshot reads node `node`: a linked node, the default
    /// sink for a system lane, or the source the microphone follows.
    fn is_read(&self, node: u32, targets: &Targets, uid: Option<&str>) -> bool {
        let is = |found: Option<(u32, &NodeEntry)>| found.is_some_and(|(id, _)| id == node);
        [&targets.mic, &targets.output]
            .into_iter()
            .flatten()
            .any(|endpoint| endpoint.node == node)
            || targets.output.is_some() && is(self.default_sink_node())
            || targets.mic.is_some() && is(self.followed_source(uid))
    }

    /// A property of the `default` metadata on subject 0 (`key` `None`
    /// clears all, `value` `None` removes one; values are JSON). True when
    /// a default this module reads changed.
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

    /// The serials of node `node` and its `ports`; `None` when one is gone.
    fn serials(&self, node: u32, ports: &[u32]) -> Option<Vec<Option<u64>>> {
        let mut serials = vec![self.nodes.get(&node)?.serial];
        for port in ports {
            serials.push(self.ports.get(port).filter(|p| p.node == node)?.serial);
        }
        Some(serials)
    }

    /// Whether `endpoint` is still in the graph as `resolve` found it, by
    /// name and serials.
    fn is_alive(&self, endpoint: &Endpoint) -> bool {
        self.nodes
            .get(&endpoint.node)
            .is_some_and(|node| node.name == endpoint.name)
            && self.serials(endpoint.node, &endpoint.ports).as_ref() == Some(&endpoint.serials)
    }

    /// The node named `uid`, or the default source.
    fn source_named(&self, uid: Option<&str>) -> Option<(u32, &NodeEntry)> {
        let name = uid.or(self.default_source.as_deref())?;
        self.nodes
            .iter()
            .find(|(_, node)| node.name == name && node.is_source())
            .map(|(id, node)| (*id, node))
    }

    fn default_sink_node(&self) -> Option<(u32, &NodeEntry)> {
        let name = self.default_sink.as_deref()?;
        self.nodes
            .iter()
            .find(|(_, node)| node.name == name && node.is_sink())
            .map(|(id, node)| (*id, node))
    }

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
        Ok(self.endpoint(node, entry, vec![port], Some(port)))
    }

    /// `uid` when it names a source [`Self::inputs`] lists, else `None`
    /// (the default): a stale UID does not fail the capture.
    pub fn known_source<'a>(&self, uid: Option<&'a str>) -> Option<&'a str> {
        uid.filter(|&uid| self.mic(Some(uid)).is_ok())
    }

    /// Whether `uid` names a Bluetooth source, which a probe must not link.
    pub fn is_bluetooth_source(&self, uid: &str) -> bool {
        self.source_named(Some(uid))
            .is_some_and(|(_, node)| node.bluetooth)
    }

    /// The source a capture asked for `uid` records now: the chosen one
    /// while recordable, else the default. The chosen source coming back
    /// reads as `DefaultInputChanged`, whose rebuild records it again.
    fn followed_source(&self, uid: Option<&str>) -> Option<(u32, &NodeEntry)> {
        self.source_named(self.known_source(uid))
    }

    /// The microphone a capture records, named by `node.description`, else
    /// `node.nick`, else `None`; a fallback when `uid` names no recordable
    /// source.
    pub fn input(&self, targets: &Targets, uid: Option<&str>) -> Option<CaptureInput> {
        let mic = targets.mic.as_ref()?;
        Some(CaptureInput {
            uid: mic.name.clone(),
            name: self
                .nodes
                .get(&mic.node)
                .and_then(|node| node.description.clone()),
            is_fallback: uid.is_some() && self.known_source(uid).is_none(),
        })
    }

    /// The input picker's devices; channels are the ports the capture could
    /// take.
    pub fn inputs(&self) -> Vec<AudioDeviceInfo> {
        self.nodes
            .iter()
            .filter(|(_, node)| self.known_source(Some(&node.name)).is_some())
            .map(|(&id, node)| {
                let outputs = self.ports_of(id, |p| p.output);
                let captures = outputs.iter().filter(|(_, p)| !p.monitor).count();
                AudioDeviceInfo {
                    id,
                    uid: node.name.clone(),
                    name: node
                        .description
                        .clone()
                        .unwrap_or_else(|| node.name.clone()),
                    input_channels: if captures > 0 {
                        captures
                    } else {
                        outputs.len()
                    },
                    output_channels: 0,
                    nominal_sample_rate: 0.0,
                    transport_type: node.media_class.clone(),
                    is_running_somewhere: false,
                    is_default_input: self.default_source.as_deref() == Some(node.name.as_str()),
                    is_default_output: false,
                    is_default_system_output: false,
                }
            })
            .collect()
    }

    fn endpoint(
        &self,
        node: u32,
        entry: &NodeEntry,
        ports: Vec<u32>,
        latency_port: Option<u32>,
    ) -> Endpoint {
        Endpoint {
            node,
            name: entry.name.clone(),
            serials: self.serials(node, &ports).unwrap_or_default(),
            ports,
            latency_port,
        }
    }

    /// The default sink's monitor endpoint: front left and right first, at
    /// most two ports.
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
        let ports = monitors.iter().take(2).map(|(id, _)| *id).collect();
        Ok(self.endpoint(node, entry, ports, latency_port))
    }

    /// What a capture of `lanes` links: endpoints, the port feeding each
    /// channel, and the buffer layout; an `…Unavailable` error when a
    /// lane's device does not resolve.
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

    /// The capture stream's input ports on node `stream`, in channel order,
    /// once all `channels` exist.
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

    /// The devices as they are now for a capture of `targets` asked for
    /// `uid`; a lane in `lost` reads as gone. The default sink stands for
    /// both outputs (the adapter resamples), so `default_output_uid` stays
    /// `None` and `sample_rate` [`SAMPLE_RATE`].
    pub fn snapshot(&self, targets: &Targets, uid: Option<&str>, lost: Lost) -> DeviceSnapshot {
        let alive = |endpoint: &Option<Endpoint>, lost: bool| {
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
                .and_then(|_| self.followed_source(uid))
                .map(|(_, node)| node.name.clone()),
            output_alive: alive(&targets.output, lost.output),
            input_alive: alive(&targets.mic, lost.mic),
            sample_rate: SAMPLE_RATE,
        }
    }
}

fn serial<'a>(props: &impl Fn(&str) -> Option<&'a str>) -> Option<u64> {
    props("object.serial").and_then(|v| v.parse().ok())
}

/// The `name` of a `default` metadata value.
fn default_name(value: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(value).ok()?;
    json.get("name")?.as_str().map(str::to_owned)
}

/// The lower bound of a port's `SPA_PARAM_Latency` in one direction.
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
    /// The latency in frames at [`SAMPLE_RATE`] for a graph cycle of
    /// `cycle_frames` (resampled to 48 kHz) at `graph_rate`. Rounded down:
    /// the far-end delay must not overshoot.
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

    const BUILT_IN_MIC: &str = "alsa_input.pci.analog-stereo";

    fn node(graph: &mut Graph, id: u32, serial: u64, name: &str, class: &str) {
        let serial = serial.to_string();
        graph.add_node(
            id,
            props(&[
                ("node.name", name),
                ("media.class", class),
                ("object.serial", &serial),
            ]),
        );
    }

    /// Port `id` of `node`; a `monitor_` prefix on `channel` makes it a
    /// monitor port.
    fn port(graph: &mut Graph, id: u32, serial: u64, node: u32, dir: &str, channel: &str) {
        let monitor = if channel.starts_with("monitor_") {
            "true"
        } else {
            "false"
        };
        let channel = channel.trim_start_matches("monitor_");
        let index = match channel {
            "FR" => "1",
            _ => "0",
        };
        let (node, serial) = (node.to_string(), serial.to_string());
        graph.add_port(
            id,
            props(&[
                ("node.id", &node),
                ("port.direction", dir),
                ("port.monitor", monitor),
                ("audio.channel", channel),
                ("port.id", index),
                ("object.serial", &serial),
            ]),
        );
    }

    /// A laptop: a stereo ALSA sink, a stereo built-in microphone, a mono
    /// USB headset microphone, the defaults set; serials are ids plus 1000.
    fn laptop() -> Graph {
        let mut graph = Graph::default();
        let sink = "alsa_output.pci.analog-stereo";
        node(&mut graph, 40, 1040, sink, "Audio/Sink");
        node(&mut graph, 41, 1041, BUILT_IN_MIC, "Audio/Source");
        node(
            &mut graph,
            42,
            1042,
            "alsa_input.usb-headset.mono",
            "Audio/Source",
        );
        port(&mut graph, 50, 1050, 40, "in", "FL");
        port(&mut graph, 51, 1051, 40, "in", "FR");
        // Monitor right before left, as the registry may announce them.
        port(&mut graph, 52, 1052, 40, "out", "monitor_FR");
        port(&mut graph, 53, 1053, 40, "out", "monitor_FL");
        port(&mut graph, 54, 1054, 41, "out", "FR");
        port(&mut graph, 55, 1055, 41, "out", "FL");
        port(&mut graph, 56, 1056, 42, "out", "MONO");
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
        node(&mut graph, 60, 1060, "virtual-mic", "Audio/Source/Virtual");
        port(&mut graph, 61, 1061, 60, "in", "MONO");
        port(&mut graph, 62, 1062, 60, "out", "monitor_MONO");
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
    fn the_picker_lists_the_sources_a_microphone_can_record() {
        let mut graph = laptop();
        node(&mut graph, 60, 1060, "virtual-mic", "Audio/Source/Virtual");
        port(&mut graph, 61, 1061, 60, "in", "MONO");
        port(&mut graph, 62, 1062, 60, "out", "monitor_MONO");
        // A source without ports yet: nothing to record.
        node(&mut graph, 63, 1063, "portless-mic", "Audio/Source");
        node(&mut graph, 64, 1064, "an-app", "Stream/Input/Audio");
        port(&mut graph, 65, 1065, 64, "in", "MONO");
        graph.add_node(
            66,
            props(&[
                ("node.name", "usb-mic"),
                ("node.description", "USB Microphone"),
                ("node.nick", "Mic"),
                ("media.class", "Audio/Source"),
            ]),
        );
        port(&mut graph, 67, 1067, 66, "out", "MONO");
        graph.add_node(
            68,
            props(&[
                ("node.name", "nick-mic"),
                ("node.nick", "Nick"),
                ("media.class", "Audio/Source"),
            ]),
        );
        port(&mut graph, 69, 1069, 68, "out", "MONO");
        let inputs = graph.inputs();
        let listed: Vec<_> = inputs
            .iter()
            .map(|d| (d.id, d.uid.as_str(), d.name.as_str(), d.input_channels))
            .collect();
        assert_eq!(
            listed,
            vec![
                (41, BUILT_IN_MIC, BUILT_IN_MIC, 2),
                (
                    42,
                    "alsa_input.usb-headset.mono",
                    "alsa_input.usb-headset.mono",
                    1
                ),
                (60, "virtual-mic", "virtual-mic", 1),
                (66, "usb-mic", "USB Microphone", 1),
                (68, "nick-mic", "Nick", 1),
            ],
            "sources only, by id, named by description, else nick, else UID"
        );
        let defaults: Vec<_> = inputs.iter().map(|d| d.is_default_input).collect();
        assert_eq!(defaults, vec![true, false, false, false, false]);
        assert!(inputs.iter().all(|d| d.is_input() && !d.is_output()));
        assert_eq!(inputs[2].transport_type, "Audio/Source/Virtual");
    }

    #[test]
    fn a_uid_that_names_no_recordable_source_falls_back_to_the_default() {
        let mut graph = laptop();
        node(&mut graph, 63, 1063, "portless-mic", "Audio/Source");
        let headset = Some("alsa_input.usb-headset.mono");
        assert_eq!(graph.known_source(headset), headset);
        assert_eq!(graph.known_source(None), None);
        for unknown in [
            "BuiltInMicrophoneDevice",
            "alsa_output.pci.analog-stereo",
            "portless-mic",
        ] {
            assert_eq!(graph.known_source(Some(unknown)), None, "{unknown}");
        }
        let asked = Some("BuiltInMicrophoneDevice");
        let targets = graph
            .resolve(&[AudioLane::Mixed], graph.known_source(asked))
            .unwrap();
        assert_eq!(targets.feeds, vec![(41, 55)], "the default source");
        assert_eq!(
            graph.input(&targets, asked),
            Some(CaptureInput {
                uid: BUILT_IN_MIC.to_owned(),
                name: None,
                is_fallback: true,
            })
        );
        let baseline = graph.snapshot(&targets, asked, Lost::NONE);
        assert_eq!(baseline.input_uid.as_deref(), Some(BUILT_IN_MIC));
        graph.set_default(
            Some(DEFAULT_SOURCE_KEY),
            Some(r#"{"name":"alsa_input.usb-headset.mono"}"#),
        );
        assert_eq!(
            graph
                .snapshot(&targets, asked, Lost::NONE)
                .difference(&baseline),
            Some(crate::capture::DeviceChangeReason::DefaultInputChanged),
            "a capture that fell back follows the default, as one with no UID"
        );
    }

    #[test]
    fn a_chosen_source_that_comes_back_is_a_change_once_it_can_be_recorded() {
        let mut graph = laptop();
        let asked = Some("usb-mic-later");
        let targets = graph
            .resolve(&[AudioLane::Mixed], graph.known_source(asked))
            .unwrap();
        graph.track(&targets, asked);
        let baseline = graph.snapshot(&targets, asked, Lost::NONE);
        assert_eq!(baseline.input_uid.as_deref(), Some(BUILT_IN_MIC));
        let judged = |graph: &Graph| {
            graph
                .snapshot(&targets, asked, Lost::NONE)
                .difference(&baseline)
        };
        assert!(
            !graph.add_node(
                90,
                props(&[("node.name", "other-mic"), ("media.class", "Audio/Source")])
            ),
            "another source is no change"
        );
        assert!(graph.add_node(
            91,
            props(&[
                ("node.name", "usb-mic-later"),
                ("media.class", "Audio/Source"),
                ("object.serial", "1091"),
            ])
        ));
        assert_eq!(judged(&graph), None, "no port to record yet");
        assert!(graph.add_port(
            92,
            props(&[
                ("node.id", "91"),
                ("port.direction", "out"),
                ("audio.channel", "MONO"),
                ("port.id", "0"),
                ("object.serial", "1092"),
            ])
        ));
        assert_eq!(
            judged(&graph),
            Some(crate::capture::DeviceChangeReason::DefaultInputChanged),
            "the rebuild records it again"
        );
        assert!(graph.remove(92), "the followed source's port");
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
        node(&mut graph, 1, 1001, "mono-sink", "Audio/Sink");
        graph.set_default(Some(DEFAULT_SINK_KEY), Some(r#"{"name":"mono-sink"}"#));
        assert!(matches!(
            graph.resolve(&[AudioLane::System], None),
            Err(CaptureError::UnexpectedStreamLayout(_))
        ));
        port(&mut graph, 2, 1002, 1, "out", "monitor_MONO");
        let targets = graph.resolve(&[AudioLane::System], None).unwrap();
        assert_eq!(targets.feeds, vec![(1, 2)]);
        assert_eq!(targets.layout.sources[0].right, None);
        assert_eq!(targets.output.unwrap().latency_port, None);
    }

    #[test]
    fn stream_ports_wait_for_every_channel() {
        let mut graph = laptop();
        port(&mut graph, 92, 1092, 90, "in", "AUX1");
        assert_eq!(graph.stream_ports(90, 2), None);
        port(&mut graph, 91, 1091, 90, "in", "AUX0");
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
        let baseline = graph.snapshot(&targets, None, Lost::NONE);
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
            graph
                .snapshot(&targets, None, Lost::NONE)
                .difference(&baseline),
            None
        );

        graph.set_default(
            Some(DEFAULT_SOURCE_KEY),
            Some(r#"{"name":"alsa_input.usb-headset.mono"}"#),
        );
        assert_eq!(
            graph
                .snapshot(&targets, None, Lost::NONE)
                .difference(&baseline),
            Some(crate::capture::DeviceChangeReason::DefaultInputChanged)
        );
        assert_eq!(
            graph
                .snapshot(&targets, None, Lost::ALL)
                .difference(&baseline),
            Some(crate::capture::DeviceChangeReason::OutputDeviceGone),
            "a lost connection loses the output first"
        );
        assert!(graph.remove(40));
        node(&mut graph, 40, 2040, "a-new-node", "Audio/Sink");
        assert_eq!(
            graph
                .snapshot(&targets, None, Lost::NONE)
                .difference(&baseline),
            Some(crate::capture::DeviceChangeReason::OutputDeviceGone),
            "a reused id is not the device that went"
        );
    }

    #[test]
    fn a_lost_link_loses_only_the_lane_it_serves() {
        use crate::capture::DeviceChangeReason::{InputDeviceGone, OutputDeviceGone};
        let graph = laptop();
        let targets = graph
            .resolve(&[AudioLane::Mic, AudioLane::System], None)
            .unwrap();
        let mic = targets.lost_with(41, 55);
        let monitor = targets.lost_with(40, 52);
        assert_eq!(
            mic,
            Lost {
                mic: true,
                output: false
            }
        );
        assert_eq!(
            monitor,
            Lost {
                mic: false,
                output: true
            }
        );
        assert_eq!(targets.lost_with(40, 53), monitor);
        assert_eq!(targets.lost_with(41, 54), Lost::ALL, "not a linked port");
        let baseline = graph.snapshot(&targets, None, Lost::NONE);
        let judged = |lost| graph.snapshot(&targets, None, lost).difference(&baseline);
        assert_eq!(
            judged(mic),
            Some(InputDeviceGone),
            "a microphone gone during a call is not the output gone"
        );
        assert_eq!(judged(monitor), Some(OutputDeviceGone));
        assert_eq!(
            judged(mic.union(monitor)),
            Some(OutputDeviceGone),
            "both lost loses the output first, as a lost connection does"
        );
        assert_eq!(mic.union(monitor), Lost::ALL);
        assert!(!Lost::NONE.any());
    }

    #[test]
    fn in_person_a_lost_link_or_connection_reads_as_the_input_gone() {
        let graph = laptop();
        let targets = graph.resolve(&[AudioLane::Mixed], None).unwrap();
        let mic = targets.lost_with(41, 55);
        assert_eq!(
            mic,
            Lost {
                mic: true,
                output: false
            }
        );
        let baseline = graph.snapshot(&targets, None, Lost::NONE);
        for lost in [mic, Lost::ALL] {
            assert_eq!(
                graph.snapshot(&targets, None, lost).difference(&baseline),
                Some(crate::capture::DeviceChangeReason::InputDeviceGone)
            );
        }
    }

    #[test]
    fn a_device_recreated_under_its_id_and_name_reads_as_gone() {
        let mut graph = laptop();
        let targets = graph.resolve(&[AudioLane::Mixed], None).unwrap();
        let baseline = graph.snapshot(&targets, None, Lost::NONE);
        assert!(graph.remove(55));
        assert!(graph.remove(54));
        assert!(graph.remove(41));
        // WirePlumber restarted: the source is back, ids and name reused.
        node(&mut graph, 41, 2041, BUILT_IN_MIC, "Audio/Source");
        port(&mut graph, 54, 2054, 41, "out", "FR");
        port(&mut graph, 55, 2055, 41, "out", "FL");
        assert_eq!(
            graph
                .snapshot(&targets, None, Lost::NONE)
                .difference(&baseline),
            Some(crate::capture::DeviceChangeReason::InputDeviceGone),
            "Steno's link died with the old port"
        );
    }

    #[test]
    fn a_linked_port_that_goes_alone_takes_its_lane_with_it() {
        let mut graph = laptop();
        let targets = graph
            .resolve(&[AudioLane::Mic, AudioLane::System], None)
            .unwrap();
        let baseline = graph.snapshot(&targets, None, Lost::NONE);
        assert!(graph.remove(54), "a port going is a change to judge");
        assert_eq!(
            graph
                .snapshot(&targets, None, Lost::NONE)
                .difference(&baseline),
            None,
            "the microphone's second channel was not linked"
        );
        assert!(graph.remove(52));
        assert_eq!(
            graph
                .snapshot(&targets, None, Lost::NONE)
                .difference(&baseline),
            Some(crate::capture::DeviceChangeReason::OutputDeviceGone)
        );
        assert!(!graph.remove(52), "gone already");
    }

    #[test]
    fn once_tracked_only_what_the_snapshot_reads_going_is_a_change() {
        let mut graph = laptop();
        // Another app's playback stream and its port.
        node(&mut graph, 90, 1090, "an-app", "Stream/Output/Audio");
        port(&mut graph, 91, 1091, 90, "out", "FL");
        let lanes = [AudioLane::Mic, AudioLane::System];
        let targets = graph.resolve(&lanes, None).unwrap();
        graph.track(&targets, None);
        assert!(!graph.remove(91), "another app's port");
        assert!(!graph.remove(90), "another app's stream");
        assert!(graph.remove(54), "an unlinked port of the microphone");
        assert!(graph.remove(50), "a playback port of the default sink");
        // The default source moves to the headset: its going now counts,
        // and the built-in microphone, linked, still does.
        graph.set_default(
            Some(DEFAULT_SOURCE_KEY),
            Some(r#"{"name":"alsa_input.usb-headset.mono"}"#),
        );
        assert!(graph.remove(56), "the new default source's port");
        assert!(graph.remove(42), "the new default source");
        assert!(graph.remove(41), "the linked microphone");
        assert!(!graph.remove(41), "gone already");
        // The default sink moves to a headset: its going counts too.
        node(&mut graph, 43, 1043, "bluez_output.headset", "Audio/Sink");
        assert!(!graph.remove(43), "a sink nothing reads");
        node(&mut graph, 43, 1043, "bluez_output.headset", "Audio/Sink");
        graph.set_default(
            Some(DEFAULT_SINK_KEY),
            Some(r#"{"name":"bluez_output.headset"}"#),
        );
        assert!(graph.remove(43), "the new default sink");
    }

    #[test]
    fn an_explicit_microphone_tracks_only_its_own_source() {
        let mut graph = laptop();
        let uid = Some("alsa_input.usb-headset.mono");
        let targets = graph.resolve(&[AudioLane::Mixed], uid).unwrap();
        graph.track(&targets, uid);
        assert!(!graph.remove(55), "the default source, not recorded");
        assert!(!graph.remove(53), "the default sink, not recorded");
        assert!(graph.remove(56), "the headset's port");
    }

    #[test]
    fn a_microphone_prefers_a_capture_port_to_an_earlier_monitor() {
        let mut graph = Graph::default();
        node(&mut graph, 70, 1070, "duplex-mic", "Audio/Source");
        // The monitor holds `port.id` 0, the capture port 1.
        port(&mut graph, 71, 1071, 70, "out", "monitor_MONO");
        port(&mut graph, 72, 1072, 70, "out", "FR");
        let targets = graph
            .resolve(&[AudioLane::Mic], Some("duplex-mic"))
            .unwrap();
        assert_eq!(targets.feeds, vec![(70, 72)]);
    }

    #[test]
    fn a_duplex_default_output_records_only_its_monitors() {
        let mut graph = Graph::default();
        node(&mut graph, 80, 1080, "headset", "Audio/Duplex");
        port(&mut graph, 81, 1081, 80, "out", "FL");
        port(&mut graph, 82, 1082, 80, "out", "monitor_FL");
        port(&mut graph, 83, 1083, 80, "out", "monitor_FR");
        graph.set_default(Some(DEFAULT_SINK_KEY), Some(r#"{"name":"headset"}"#));
        let targets = graph.resolve(&[AudioLane::System], None).unwrap();
        assert_eq!(
            targets.feeds,
            vec![(80, 82), (80, 83)],
            "not the capture port"
        );
    }

    #[test]
    fn an_explicit_microphone_ignores_the_default_and_reports_its_loss() {
        let mut graph = laptop();
        let uid = Some("alsa_input.usb-headset.mono");
        let targets = graph.resolve(&[AudioLane::Mixed], uid).unwrap();
        let baseline = graph.snapshot(&targets, uid, Lost::NONE);
        assert_eq!(baseline.output_uid, None, "in person watches no output");
        graph.set_default(Some(DEFAULT_SOURCE_KEY), None);
        graph.set_default(Some(DEFAULT_SINK_KEY), None);
        assert_eq!(
            graph
                .snapshot(&targets, uid, Lost::NONE)
                .difference(&baseline),
            None
        );
        graph.remove(42);
        assert_eq!(
            graph
                .snapshot(&targets, uid, Lost::NONE)
                .difference(&baseline),
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
