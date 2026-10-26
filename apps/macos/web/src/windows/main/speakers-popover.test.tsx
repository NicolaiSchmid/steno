import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { MeetingDetailSnapshot } from "@/bridge/contract";
import { applyScenario, loadFixtureSnapshots } from "@/bridge/mock-transport";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { SPLIT_HINT, SpeakersPopover } from "./speakers-popover";

const ME = "00000000-0000-0000-0000-000000000014";
// The fixture's first speaker: Me in the speakers-* scenarios, Nicolai in speakers-split.
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

async function open(scenario = "") {
	const user = userEvent.setup();
	const harness = await createBridgeHarness();
	const { speakers, transcript } = await detail(scenario);
	renderWithBridge(
		<SpeakersPopover speakers={speakers} transcript={transcript} />,
		harness,
	);
	await user.click(screen.getByTestId("speakers-trigger"));
	return { user, harness, popover: await screen.findByRole("dialog") };
}

describe("SpeakersPopover", () => {
	it("lists the unnamed with a line they said, and Me without a clip", async () => {
		const { popover } = await open("speakers-none");
		expect(within(popover).getByText("0 of 3 named")).toBeInTheDocument();
		const me = within(popover).getByTestId(`speakers-popover-row-${ME}`);
		expect(
			within(me).getByRole("button", { name: "Name yourself" }),
		).toBeInTheDocument();
		expect(within(me).queryByRole("button", { name: /Play/ })).toBeNull();
		const other = within(popover).getByTestId(
			`speakers-popover-row-${SPEAKER_1}`,
		);
		expect(within(other).getByText(/^“.+”$/)).toBeInTheDocument();
		expect(
			within(other).getByRole("button", { name: /Play a sample/ }),
		).toBeInTheDocument();
		expect(within(popover).queryByText("Who is this?")).toBeNull();
	});

	it("names by typing and Enter, then moves on to the next speaker", async () => {
		const { user, harness } = await open("speakers-me");
		await user.click(screen.getByRole("button", { name: "Name Speaker 1" }));
		const field = screen.getByTestId(`speaker-field-${SPEAKER_1}`);
		expect(field).toHaveFocus();
		expect(field).toHaveValue("");
		const list = await screen.findByRole("listbox");
		await within(list).findAllByRole("option");
		expect(
			within(list)
				.getAllByRole("group")
				.map((group) => group.getAttribute("aria-label")),
		).toEqual(["In this meeting", "Recent people"]);
		expect(within(list).queryByText("Leave unnamed")).toBeNull();

		await user.keyboard("{ArrowDown}{Enter}");
		expect(callsTo(harness.transport, "speakers.select")).toEqual([
			{
				method: "speakers.select",
				params: {
					speakerID: SPEAKER_1,
					option: {
						kind: "person",
						label: "Nicolai",
						personID: "00000000-0000-0000-0000-00000000000A",
					},
				},
			},
		]);
	});

	it("moves to the next unnamed speaker after a pick, and Escape gives up", async () => {
		const { user, harness } = await open("speakers-none");
		await user.click(screen.getByRole("button", { name: "Name Speaker 1" }));
		await screen.findAllByRole("option");
		await user.keyboard("Jo{Enter}");
		expect(callsTo(harness.transport, "speakers.select").at(-1)).toEqual({
			method: "speakers.select",
			params: { speakerID: SPEAKER_1, option: { kind: "create", label: "Jo" } },
		});
		expect(screen.getByTestId(`speaker-field-${SPEAKER_2}`)).toHaveFocus();

		await user.keyboard("{Escape}");
		expect(screen.queryByTestId(`speaker-field-${SPEAKER_2}`)).toBeNull();
		expect(screen.getByRole("dialog")).toBeInTheDocument();
	});

	it("confirms a suggestion with its own button", async () => {
		const { user, harness, popover } = await open();
		await user.click(
			within(popover).getByTestId(`speakers-popover-confirm-${ANNA}`),
		);
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
				},
			},
		]);
	});

	it("puts a person heard as two speakers under the first, with the reason", async () => {
		const { popover } = await open("speakers-split");
		const rows = within(popover)
			.getAllByRole("listitem")
			.map((row) => row.getAttribute("data-testid"));
		expect(rows.slice(0, 2)).toEqual([
			`speakers-popover-row-${ME}`,
			`speakers-popover-row-${UNNAMED}`,
		]);
		expect(
			within(popover).getByRole("button", { name: SPLIT_HINT }),
		).toBeInTheDocument();
	});
});
