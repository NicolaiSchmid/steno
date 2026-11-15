import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { MeetingDetailSnapshot, SpeakerOption } from "@/bridge/contract";
import { loadFixtureSnapshots } from "@/bridge/mock-transport";
import { BridgeError } from "@/bridge/transport";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { SpeakerNameField, selectSpeaker } from "./speaker-name-field";
import type { Speaker } from "./speaker-names";

const UNNAMED = "00000000-0000-0000-0000-000000000017";
const NICOLAI: SpeakerOption = {
	kind: "person",
	label: "Nicolai",
	personID: "00000000-0000-0000-0000-00000000000A",
};
afterEach(() => {
	vi.restoreAllMocks();
});

const WOULD_COMBINE = "Nicolai already speaks as Speaker 1 in this meeting.";

async function unnamed(): Promise<Speaker> {
	const detail = (await loadFixtureSnapshots())[
		"meeting.detail"
	] as MeetingDetailSnapshot;
	const speaker = detail.speakers.find((row) => row.id === UNNAMED);
	if (!speaker) {
		throw new Error("fixture has no unnamed speaker");
	}
	return speaker;
}

/** `speakers.select` refusing without `combine`, as the hosts do. */
function refusingSelect(params: unknown) {
	if (!(params as { combine: boolean }).combine) {
		throw new BridgeError("speakers.select", "wouldCombine", WOULD_COMBINE);
	}
	return undefined;
}

describe("selectSpeaker", () => {
	it("sends the pick without combining", async () => {
		const harness = await createBridgeHarness();
		expect(
			await selectSpeaker(harness.client, await unnamed(), NICOLAI),
		).toEqual({ kind: "committed" });
		expect(callsTo(harness.transport, "speakers.select")).toEqual([
			{
				method: "speakers.select",
				params: { speakerID: UNNAMED, option: NICOLAI, combine: false },
			},
		]);
		expect(callsTo(harness.transport, "ui.confirmDestructive")).toEqual([]);
	});

	it("confirms a pick that would combine, then sends it with combine", async () => {
		const harness = await createBridgeHarness(
			"",
			{},
			{ "speakers.select": refusingSelect },
		);
		const speaker = await unnamed();
		expect(await selectSpeaker(harness.client, speaker, NICOLAI)).toEqual({
			kind: "committed",
		});
		expect(callsTo(harness.transport, "ui.confirmDestructive")).toEqual([
			{
				method: "ui.confirmDestructive",
				params: {
					title: `Combine ${speaker.clusterLabel} with Nicolai?`,
					message: `${WOULD_COMBINE} Their paragraphs become Nicolai’s. This cannot be undone.`,
					confirmTitle: "Combine",
				},
			},
		]);
		expect(
			callsTo(harness.transport, "speakers.select").map(
				(call) => (call.params as { combine: boolean }).combine,
			),
		).toEqual([false, true]);
	});

	it("leaves the pick when the combine is cancelled", async () => {
		const harness = await createBridgeHarness(
			"",
			{},
			{
				"speakers.select": refusingSelect,
				"ui.confirmDestructive": { confirmed: false },
			},
		);
		expect(
			await selectSpeaker(harness.client, await unnamed(), NICOLAI),
		).toEqual({ kind: "cancelled" });
		expect(callsTo(harness.transport, "speakers.select")).toHaveLength(1);
	});

	it("answers any other failure with the host's message", async () => {
		const harness = await createBridgeHarness(
			"",
			{},
			{ "speakers.select": new Error("Speaker could not be named: busy") },
		);
		vi.spyOn(console, "error").mockImplementation(() => {});
		expect(
			await selectSpeaker(harness.client, await unnamed(), NICOLAI),
		).toEqual({ kind: "failed", message: "Speaker could not be named: busy" });
		expect(callsTo(harness.transport, "ui.confirmDestructive")).toEqual([]);
	});
});

describe("SpeakerNameField", () => {
	async function field(replies: Record<string, unknown> = {}) {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("", {}, replies);
		const onDone = vi.fn();
		renderWithBridge(
			<SpeakerNameField onDone={onDone} speaker={await unnamed()} />,
			harness,
		);
		await screen.findAllByRole("option");
		const input = screen.getByTestId(`speaker-field-${UNNAMED}`);
		await user.type(input, "Nic");
		await vi.waitFor(() =>
			expect(
				screen.getByRole("option", { name: /^Nicolai/ }),
			).toBeInTheDocument(),
		);
		return { user, harness, onDone, input };
	}

	it("closes once the host took the pick", async () => {
		const { user, onDone } = await field();
		await user.click(screen.getByRole("option", { name: /^Nicolai/ }));
		await vi.waitFor(() => expect(onDone).toHaveBeenCalledWith(true));
		expect(onDone).toHaveBeenCalledTimes(1);
		expect(screen.queryByRole("alert")).toBeNull();
	});

	it("stays on the speaker with its text when the combine is cancelled", async () => {
		const { user, harness, onDone, input } = await field({
			"speakers.select": refusingSelect,
			"ui.confirmDestructive": { confirmed: false },
		});
		await user.click(screen.getByRole("option", { name: /^Nicolai/ }));
		await vi.waitFor(() => expect(input).toHaveFocus());
		expect(callsTo(harness.transport, "ui.confirmDestructive")).toHaveLength(1);
		expect(callsTo(harness.transport, "speakers.select")).toHaveLength(1);
		expect(onDone).not.toHaveBeenCalled();
		expect(input).toHaveValue("Nic");
		expect(screen.queryByRole("alert")).toBeNull();
	});

	it("stays open with the host's message when the pick failed", async () => {
		vi.spyOn(console, "error").mockImplementation(() => {});
		const { user, onDone, input } = await field({
			"speakers.select": new Error("Speaker could not be named: busy"),
		});
		await user.click(screen.getByRole("option", { name: /^Nicolai/ }));
		expect(await screen.findByRole("alert")).toHaveTextContent(
			"Speaker could not be named: busy",
		);
		expect(onDone).not.toHaveBeenCalled();
		expect(input).toHaveValue("Nic");
		expect(input).toHaveFocus();
		expect(input).toHaveAttribute("aria-invalid", "true");

		await user.type(input, "o");
		expect(screen.queryByRole("alert")).toBeNull();
	});
});
