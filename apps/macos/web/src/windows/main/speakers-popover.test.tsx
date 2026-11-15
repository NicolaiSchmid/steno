import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { MeetingDetailSnapshot } from "@/bridge/contract";
import {
	applyScenario,
	type FixtureMap,
	loadFixtureSnapshots,
} from "@/bridge/mock-transport";
import { BridgeError } from "@/bridge/transport";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { SpeakersPopover } from "./speakers-popover";

const ME = "00000000-0000-0000-0000-000000000014";
// The fixture's first speaker: Me in the speakers-* scenarios, Nicolai otherwise.
const NICOLAI = ME;
const SPEAKER_1 = "00000000-0000-0000-0000-000000000015";
const SPEAKER_2 = "00000000-0000-0000-0000-000000000016";
const ANNA = "00000000-0000-0000-0000-000000000016";
const UNNAMED = "00000000-0000-0000-0000-000000000017";

async function detail(scenario = ""): Promise<MeetingDetailSnapshot> {
	return applyScenario(
		await loadFixtureSnapshots(),
		new URLSearchParams(scenario && `scenario=${scenario}`),
	)["meeting.detail"] as MeetingDetailSnapshot;
}

afterEach(() => {
	vi.restoreAllMocks();
});

/** `speakers.select` refusing without `combine`, as the hosts do. */
function refusingSelect(params: unknown) {
	if (!(params as { combine: boolean }).combine) {
		throw new BridgeError(
			"speakers.select",
			"wouldCombine",
			"Nicolai already speaks as Speaker 1 in this meeting.",
		);
	}
	return undefined;
}

const BUSY: FixtureMap = {
	"speakers.select": new Error("Speaker could not be named: busy"),
};
const NEVER: FixtureMap = { "speakers.select": () => new Promise(() => {}) };

async function render(scenario = "", replies: FixtureMap = {}) {
	const user = userEvent.setup();
	const harness = await createBridgeHarness("", {}, replies);
	const { speakers, transcript } = await detail(scenario);
	renderWithBridge(
		<SpeakersPopover speakers={speakers} transcript={transcript} />,
		harness,
	);
	return { user, harness };
}

async function open(scenario = "", replies: FixtureMap = {}) {
	const rendered = await render(scenario, replies);
	await rendered.user.click(screen.getByTestId("speakers-trigger"));
	return { ...rendered, panel: await screen.findByTestId("speakers-panel") };
}

const row = (id: string) => screen.getByTestId(`speakers-popover-name-${id}`);

describe("the speakers pill", () => {
	it.each([
		["speakers-none", /^Name speakers$/],
		["", "1 needs a name"],
		["speakers-check", "Check 1 name"],
		["speakers-done", "Nicolai, Jérôme, Anna"],
	])("in %s reads %s", async (scenario, words) => {
		await render(scenario);
		expect(screen.getByTestId("speakers-trigger")).toHaveTextContent(words);
	});

	it("says it is finding speakers, disabled, while the pipeline diarizes", async () => {
		const harness = await createBridgeHarness();
		renderWithBridge(
			<SpeakersPopover finding speakers={[]} transcript={[]} />,
			harness,
		);
		expect(screen.getByTestId("speakers-finding")).toBeDisabled();
		expect(screen.getByTestId("speakers-finding")).toHaveTextContent(
			"Finding speakers…",
		);
	});

	it("is not there without speech", async () => {
		const harness = await createBridgeHarness();
		const { speakers } = await detail();
		renderWithBridge(
			<SpeakersPopover speakers={speakers} transcript={[]} />,
			harness,
		);
		expect(screen.queryByTestId("speakers-trigger")).toBeNull();
	});
});

describe("the speakers panel", () => {
	it("lists the work first: the voice match, then the unnamed, then the people", async () => {
		const { panel } = await open();
		expect(
			within(panel)
				.getAllByRole("heading")
				.map((heading) => heading.textContent),
		).toEqual(["Is this right?", "Need names", "Speakers"]);
		expect(row(ANNA)).toHaveTextContent("Speaker 3Sounds like Anna");
		// A voice without a clip keeps its slot, disabled, and says so.
		expect(
			within(row(UNNAMED)).getByRole("button", { name: "No clip" }),
		).toBeDisabled();
		// No email beside a confirmed name.
		expect(within(panel).queryByText(/@example\.com/)).toBeNull();
		expect(screen.queryByText(/87%/)).toBeNull();
	});

	it("puts unnamed Me above the sections, without a clip", async () => {
		await open("speakers-none");
		expect(row(ME)).toHaveTextContent("You spoke on the mic.Name yourself");
		expect(within(row(ME)).queryByRole("button")).toBeNull();
	});

	it("opens on the first row with work, one tab stop, the arrows between rows", async () => {
		const { user } = await open();
		expect(row(ANNA)).toHaveFocus();
		expect(row(ANNA)).toHaveAttribute("tabindex", "0");
		expect(row(UNNAMED)).toHaveAttribute("tabindex", "-1");
		expect(
			within(row(ANNA))
				.getAllByRole("button")
				.every((button) => button.getAttribute("tabindex") === "-1"),
		).toBe(true);
		await user.keyboard("{ArrowDown}");
		expect(row(UNNAMED)).toHaveFocus();
		await user.keyboard("{ArrowUp}");
		expect(row(ANNA)).toHaveFocus();
	});

	it("confirms with Y and plays with P on a row", async () => {
		const { user, harness } = await open();
		await user.keyboard("p");
		expect(callsTo(harness.transport, "speakers.play")).toEqual([
			{ method: "speakers.play", params: { speakerID: ANNA } },
		]);
		await user.keyboard("y");
		expect(callsTo(harness.transport, "speakers.select")).toEqual([
			{
				method: "speakers.select",
				params: {
					speakerID: ANNA,
					option: {
						kind: "person",
						label: "Anna",
						personID: "00000000-0000-0000-0000-00000000000C",
					},
					combine: false,
				},
			},
		]);
	});

	it("names with the evidence on screen, then moves on once the host took it", async () => {
		const { user, harness } = await open("speakers-none");
		await user.click(row(SPEAKER_1));
		const field = screen.getByTestId(`speaker-field-${SPEAKER_1}`);
		expect(field).toHaveFocus();
		expect(field).toHaveAttribute("placeholder", "Speaker 1");
		// The label, the line said and the play button stay.
		expect(row(SPEAKER_1)).toHaveTextContent("Speaker 1");
		expect(
			screen.getByTestId(`speakers-popover-snippet-${SPEAKER_1}`),
		).toBeVisible();
		expect(
			screen.getByTestId(`speakers-popover-play-${SPEAKER_1}`),
		).toBeVisible();

		await screen.findAllByRole("option");
		await user.keyboard("{Enter}");
		expect(callsTo(harness.transport, "speakers.select")).toHaveLength(1);
		await vi.waitFor(() =>
			expect(screen.getByTestId(`speaker-field-${SPEAKER_2}`)).toHaveFocus(),
		);
	});

	it("shows Saving and does not move on before the host answers", async () => {
		const { user } = await open("speakers-none", NEVER);
		await user.click(row(SPEAKER_1));
		await screen.findAllByRole("option");
		await user.keyboard("{Enter}");
		expect(
			await screen.findByTestId(`speakers-popover-saving-${SPEAKER_1}`),
		).toHaveTextContent("Saving");
		expect(row(SPEAKER_1)).toHaveTextContent("Anna");
		expect(screen.queryByRole("combobox")).toBeNull();
	});

	it("reopens a failed pick with its text and a red line with Retry", async () => {
		vi.spyOn(console, "error").mockImplementation(() => {});
		const { user, harness } = await open("speakers-none", BUSY);
		await user.click(row(SPEAKER_1));
		await screen.findAllByRole("option");
		await user.keyboard("Jo");
		await vi.waitFor(() =>
			expect(screen.getByRole("option", { name: /Create/ })).toBeVisible(),
		);
		await user.keyboard("{Enter}");
		const alert = await screen.findByTestId(
			`speakers-popover-failure-${SPEAKER_1}`,
		);
		expect(alert).toHaveAttribute("role", "alert");
		expect(alert).toHaveTextContent("Speaker could not be named: busy");
		expect(screen.getByTestId(`speaker-field-${SPEAKER_1}`)).toHaveValue("Jo");
		expect(screen.queryByTestId(`speaker-field-${SPEAKER_2}`)).toBeNull();

		await user.click(within(alert).getByRole("button", { name: "Retry" }));
		expect(callsTo(harness.transport, "speakers.select")).toHaveLength(2);
	});

	it("starts the next visit clean when the panel closed while a pick saved", async () => {
		vi.spyOn(console, "error").mockImplementation(() => {});
		let refuse: (cause: Error) => void = () => {};
		const { user } = await open("speakers-none", {
			"speakers.select": () =>
				new Promise((_, reject) => {
					refuse = reject;
				}),
		});
		await user.click(row(SPEAKER_1));
		await screen.findAllByRole("option");
		await user.keyboard("{Enter}");
		await screen.findByTestId(`speakers-popover-saving-${SPEAKER_1}`);
		await user.keyboard("{Escape}");
		await vi.waitFor(() =>
			expect(screen.queryByTestId("speakers-panel")).toBeNull(),
		);
		refuse(new Error("Speaker could not be named: busy"));
		await user.click(screen.getByTestId("speakers-trigger"));
		await screen.findByTestId("speakers-panel");
		expect(screen.queryByRole("combobox")).toBeNull();
		expect(screen.queryByRole("alert")).toBeNull();
	});

	it("closes the field with Escape, then the panel, focus going back each time", async () => {
		const { user } = await open("speakers-none");
		await user.click(row(SPEAKER_1));
		await user.keyboard("{Escape}");
		expect(screen.queryByRole("combobox")).toBeNull();
		expect(row(SPEAKER_1)).toHaveFocus();
		await user.keyboard("{Escape}");
		await vi.waitFor(() =>
			expect(screen.queryByTestId("speakers-panel")).toBeNull(),
		);
		expect(screen.getByTestId("speakers-trigger")).toHaveFocus();
	});

	it("asks before combining a voice into a person, then combines", async () => {
		const { user, harness } = await open("speakers-split", {
			"speakers.select": refusingSelect,
		});
		expect(row(UNNAMED)).toHaveTextContent("Also Nicolai?Speaker 4");
		expect(screen.getByTestId(`speakers-nested-${NICOLAI}`)).toContainElement(
			row(UNNAMED),
		);
		await user.click(screen.getByRole("button", { name: "Same person" }));
		const dialog = await screen.findByTestId("confirm-dialog");
		expect(dialog).toHaveTextContent("Combine Speaker 4 with Nicolai?");
		expect(dialog).toHaveTextContent("This cannot be undone.");
		await user.click(within(dialog).getByRole("button", { name: "Combine" }));
		await vi.waitFor(() =>
			expect(
				callsTo(harness.transport, "speakers.select").map(
					(call) => (call.params as { combine: boolean }).combine,
				),
			).toEqual([false, true]),
		);
	});

	it("leaves the voice when the combine is cancelled", async () => {
		const { user, harness } = await open("speakers-split", {
			"speakers.select": refusingSelect,
		});
		await user.click(screen.getByRole("button", { name: "Same person" }));
		const dialog = await screen.findByTestId("confirm-dialog");
		await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
		await vi.waitFor(() =>
			expect(screen.queryByTestId("confirm-dialog")).toBeNull(),
		);
		expect(callsTo(harness.transport, "speakers.select")).toHaveLength(1);
		expect(screen.getByTestId("speakers-panel")).toBeInTheDocument();
		expect(screen.queryByRole("combobox")).toBeNull();
		expect(screen.getByRole("button", { name: "Same person" })).toBeVisible();
	});

	it("opens the field without the person on Someone else", async () => {
		const { user } = await open("speakers-split");
		await user.click(screen.getByRole("button", { name: "Someone else" }));
		await screen.findAllByRole("option");
		expect(screen.queryByRole("option", { name: /Nicolai/ })).toBeNull();
	});
});
