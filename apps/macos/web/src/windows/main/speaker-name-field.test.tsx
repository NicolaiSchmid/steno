import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
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
		await selectSpeaker(harness.client, await unnamed(), NICOLAI);
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
		await selectSpeaker(harness.client, speaker, NICOLAI);
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
		await selectSpeaker(harness.client, await unnamed(), NICOLAI);
		expect(callsTo(harness.transport, "speakers.select")).toHaveLength(1);
	});

	it("rejects with any other failure", async () => {
		const harness = await createBridgeHarness(
			"",
			{},
			{ "speakers.select": new Error("Speaker could not be named: busy") },
		);
		await expect(
			selectSpeaker(harness.client, await unnamed(), NICOLAI),
		).rejects.toThrow("Speaker could not be named: busy");
		expect(callsTo(harness.transport, "ui.confirmDestructive")).toEqual([]);
	});
});

describe("SpeakerNameField", () => {
	it("hands a failed pick's message to onError", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness(
			"",
			{},
			{ "speakers.select": new Error("Speaker could not be named: busy") },
		);
		const onError = vi.fn();
		renderWithBridge(
			<SpeakerNameField
				onDone={() => {}}
				onError={onError}
				speaker={await unnamed()}
			/>,
			harness,
		);
		await screen.findAllByRole("option");
		await user.click(screen.getByRole("option", { name: /^Nicolai/ }));
		await vi.waitFor(() =>
			expect(onError).toHaveBeenCalledWith("Speaker could not be named: busy"),
		);
	});
});
