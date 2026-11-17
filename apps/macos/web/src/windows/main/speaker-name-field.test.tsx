import { fireEvent, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
	MeetingDetailSnapshot,
	SpeakerOption,
	SpeakerOptionsReply,
} from "@/bridge/contract";
import {
	answerSpeakerOptions,
	type FixtureMap,
	loadFixtureReplies,
	loadFixtureSnapshots,
} from "@/bridge/mock-transport";
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

afterEach(() => {
	vi.restoreAllMocks();
});

async function meeting(): Promise<MeetingDetailSnapshot> {
	return (await loadFixtureSnapshots())[
		"meeting.detail"
	] as MeetingDetailSnapshot;
}

async function unnamed(): Promise<Speaker> {
	const speaker = (await meeting()).speakers.find((row) => row.id === UNNAMED);
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
		const confirm = vi.fn();
		expect(
			await selectSpeaker(harness.client, await unnamed(), NICOLAI, confirm),
		).toEqual({ kind: "committed" });
		expect(callsTo(harness.transport, "speakers.select")).toEqual([
			{
				method: "speakers.select",
				params: { speakerID: UNNAMED, option: NICOLAI, combine: false },
			},
		]);
		expect(confirm).not.toHaveBeenCalled();
	});

	it("confirms a pick that would combine, naming the cost, then combines", async () => {
		const harness = await createBridgeHarness(
			"",
			{},
			{ "speakers.select": refusingSelect },
		);
		const speaker = await unnamed();
		const confirm = vi.fn().mockResolvedValue(true);
		expect(
			await selectSpeaker(harness.client, speaker, NICOLAI, confirm),
		).toEqual({ kind: "committed" });
		expect(confirm).toHaveBeenCalledWith({
			title: `Combine ${speaker.clusterLabel} with Nicolai?`,
			consequence: `${WOULD_COMBINE} ${speaker.clusterLabel}’s paragraphs become Nicolai’s. This cannot be undone.`,
		});
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
			{ "speakers.select": refusingSelect },
		);
		expect(
			await selectSpeaker(
				harness.client,
				await unnamed(),
				NICOLAI,
				vi.fn().mockResolvedValue(false),
			),
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
		const confirm = vi.fn();
		expect(
			await selectSpeaker(harness.client, await unnamed(), NICOLAI, confirm),
		).toEqual({ kind: "failed", message: "Speaker could not be named: busy" });
		expect(confirm).not.toHaveBeenCalled();
	});
});

describe("SpeakerNameField", () => {
	async function field(
		options: (params: unknown, recorded?: SpeakerOptionsReply) => unknown = (
			params,
			recorded,
		) => answerSpeakerOptions(recorded, params),
		speakers?: readonly Speaker[],
	) {
		const user = userEvent.setup();
		const recorded = (await loadFixtureReplies())["speakers.options"] as
			| SpeakerOptionsReply
			| undefined;
		const replies: FixtureMap = {
			"speakers.options": (params: unknown) => options(params, recorded),
		};
		const harness = await createBridgeHarness("", {}, replies);
		const onSubmit = vi.fn();
		const onCancel = vi.fn();
		const speaker = await unnamed();
		renderWithBridge(
			<SpeakerNameField
				onCancel={onCancel}
				onSubmit={onSubmit}
				speaker={speaker}
				speakers={speakers ?? (await meeting()).speakers}
			/>,
			harness,
		);
		const input = screen.getByTestId(`speaker-field-${UNNAMED}`);
		return { user, harness, onSubmit, onCancel, input };
	}

	it("opens empty with the voice's label as the placeholder", async () => {
		const { input } = await field();
		expect(input).toHaveValue("");
		expect(input).toHaveAttribute("placeholder", "Speaker 4");
		expect(input).toHaveFocus();
	});

	it("groups the options and says which voice a pick would combine with", async () => {
		await field();
		const options = await screen.findAllByRole("option");
		expect(options.map((option) => option.textContent)).toEqual([
			"AAnnaSounds like",
			"NNicolai · combines with Speaker 1",
		]);
		expect(
			screen
				.getAllByRole("group")
				.map((group) => group.getAttribute("aria-label")),
		).toEqual(["Suggested", "Also speaking here"]);
	});

	it("does nothing on Enter while the options for the typed text load", async () => {
		const { user, onSubmit, input } = await field((params, recorded) =>
			(params as { query: string }).query
				? new Promise(() => {})
				: answerSpeakerOptions(recorded, params),
		);
		await screen.findAllByRole("option");
		await user.type(input, "Ann{Enter}");
		expect(screen.getByTestId("speaker-options-searching")).toHaveTextContent(
			"Searching…",
		);
		expect(screen.queryByRole("option")).toBeNull();
		expect(onSubmit).not.toHaveBeenCalled();
	});

	it("acts on Enter only on the highlighted row it shows", async () => {
		const { user, onSubmit, input } = await field();
		await user.type(input, "Ann");
		await vi.waitFor(() =>
			expect(screen.getByRole("option", { name: /Create/ })).toBeVisible(),
		);
		expect(screen.getByRole("option", { name: /Anna/ })).toHaveAttribute(
			"aria-selected",
			"true",
		);
		await user.keyboard("{ArrowDown}{Enter}");
		expect(onSubmit).toHaveBeenCalledWith(
			{ kind: "create", label: "Ann" },
			"Ann",
		);
	});

	it("marks the keys' highlight for the inset focus line, not the pointer's", async () => {
		const { user } = await field();
		const [first, second] = await screen.findAllByRole("option");
		const list = screen.getByRole("listbox");
		expect(list).toHaveAttribute("data-keys");
		await user.hover(second as HTMLElement);
		expect(list).not.toHaveAttribute("data-keys");
		await user.keyboard("{ArrowUp}");
		expect(list).toHaveAttribute("data-keys");
		expect(first).toHaveAttribute("data-highlighted");
	});

	it("keeps a highlighted Create row when the options fail to load", async () => {
		vi.spyOn(console, "error").mockImplementation(() => {});
		const { user, onSubmit, input } = await field((params, recorded) => {
			if ((params as { query: string }).query) {
				throw new BridgeError("speakers.options", "failed", "busy");
			}
			return answerSpeakerOptions(recorded, params);
		});
		await user.type(input, "Ann");
		expect(await screen.findByRole("alert")).toHaveTextContent(
			"People could not be loaded",
		);
		const create = screen.getByRole("option", { name: "Create “Ann”" });
		expect(create).toHaveAttribute("aria-selected", "true");
		await user.keyboard("{Enter}");
		expect(onSubmit).toHaveBeenCalledWith(
			{ kind: "create", label: "Ann" },
			"Ann",
		);
	});

	it("leaves Home and End to the caret, and a composing Enter alone", async () => {
		const { user, onSubmit, input } = await field();
		await user.type(input, "Ann");
		await vi.waitFor(() =>
			expect(screen.getByRole("option", { name: /Create/ })).toBeVisible(),
		);
		await user.keyboard("{End}");
		expect(screen.getByRole("option", { name: /Anna/ })).toHaveAttribute(
			"aria-selected",
			"true",
		);
		fireEvent.keyDown(input, { key: "Enter", isComposing: true });
		expect(onSubmit).not.toHaveBeenCalled();
	});

	it("gives up on Escape", async () => {
		const { user, onCancel } = await field();
		await user.keyboard("{Escape}");
		expect(onCancel).toHaveBeenCalledTimes(1);
	});
});
