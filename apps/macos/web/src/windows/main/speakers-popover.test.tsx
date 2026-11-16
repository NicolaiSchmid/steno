import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { MeetingDetailSnapshot } from "@/bridge/contract";
import { BridgeProvider } from "@/bridge/hooks";
import {
	applyScenario,
	type FixtureMap,
	loadFixtureSnapshots,
} from "@/bridge/mock-transport";
import { BridgeError } from "@/bridge/transport";
import * as ui from "@/components/ui";
import { TooltipProvider } from "@/components/ui";
import { PlatformProvider, SWIFT_MAC } from "@/lib/platform";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import type { Speaker } from "./speaker-names";
import { SpeakersPopover } from "./speakers-popover";

const ME = "00000000-0000-0000-0000-000000000014";
// The fixture's first speaker: Me in the speakers-* scenarios, Nicolai otherwise.
const NICOLAI = ME;
const SPEAKER_1 = "00000000-0000-0000-0000-000000000015";
const SPEAKER_2 = "00000000-0000-0000-0000-000000000016";
const ANNA = "00000000-0000-0000-0000-000000000016";
const UNNAMED = "00000000-0000-0000-0000-000000000017";
// Speaker 5, combined into Nicolai's voice in the fixture.
const COMBINED = "00000000-0000-0000-0000-000000000018";

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
	const { rerender } = renderWithBridge(
		<SpeakersPopover speakers={speakers} transcript={transcript} />,
		harness,
	);
	/** The host republishing the meeting with these speakers. */
	const republish = (next: MeetingDetailSnapshot["speakers"]) =>
		rerender(
			<PlatformProvider platform={SWIFT_MAC}>
				<TooltipProvider delay={0}>
					<BridgeProvider client={harness.client}>
						<SpeakersPopover speakers={next} transcript={transcript} />
					</BridgeProvider>
				</TooltipProvider>
			</PlatformProvider>,
		);
	return { user, harness, speakers, republish };
}

/** `speaker` as the host republishes it once confirmed as `name`. */
function named(speaker: Speaker, name: string): Speaker {
	const { suggestionName: _name, ...rest } = speaker;
	return {
		...rest,
		assignment: "confirmed",
		displayName: name,
		personID: speaker.personID ?? `person-${name}`,
	};
}

/** Picks that wait for the test to settle them, one per voice. */
function heldSelect() {
	const held = new Map<string, () => void>();
	const replies: FixtureMap = {
		"speakers.select": (params: unknown) =>
			new Promise<undefined>((resolve) => {
				held.set((params as { speakerID: string }).speakerID, () =>
					resolve(undefined),
				);
			}),
	};
	return { held, replies };
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
		// Speaker 4 needs a name and Anna's suggestion a check: both count.
		["", "2 need names"],
		["speakers-check", "Check 1 name"],
		["speakers-done", "Nicolai, Jérôme, Anna"],
	])("in %s reads %s", async (scenario, words) => {
		await render(scenario);
		expect(screen.getByTestId("speakers-trigger")).toHaveTextContent(words);
	});

	it("reads No speakers, with nothing to do, when no voice is a person", async () => {
		const harness = await createBridgeHarness();
		const { speakers, transcript } = await detail("speakers-dismissed");
		renderWithBridge(
			<SpeakersPopover
				speakers={speakers
					.filter((speaker) => speaker.clusterLabel !== "Me")
					.map((speaker) => ({ ...speaker, dismissed: true }))}
				transcript={transcript}
			/>,
			harness,
		);
		const pill = screen.getByTestId("speakers-trigger");
		expect(pill).toHaveTextContent(/^No speakers$/);
		expect(pill.querySelector("[aria-label='Unnamed']")).toBeNull();
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
		// A voice without a clip keeps its slot, disabled, and says so on screen.
		expect(
			within(row(UNNAMED)).getByRole("button", { name: "No sample" }),
		).toBeDisabled();
		expect(row(UNNAMED)).toHaveTextContent("No sample");
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

	it("combines a voice into a person without asking, and Undo separates it again", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness(
			"",
			{},
			{ "speakers.select": refusingSelect },
		);
		const { speakers, transcript } = await detail("speakers-split");
		renderWithBridge(
			<>
				<SpeakersPopover speakers={speakers} transcript={transcript} />
				<ui.ToastRegion />
			</>,
			harness,
		);
		await user.click(screen.getByTestId("speakers-trigger"));
		expect(row(UNNAMED)).toHaveTextContent("Also Nicolai?Speaker 4");
		await user.click(screen.getByRole("button", { name: "Same person" }));
		await vi.waitFor(() =>
			expect(
				callsTo(harness.transport, "speakers.select").map(
					(call) => (call.params as { combine: boolean }).combine,
				),
			).toEqual([false, true]),
		);
		expect(screen.queryByTestId("confirm-dialog")).toBeNull();
		const toast = await screen.findByTestId("toast");
		expect(toast).toHaveTextContent("Speaker 4 combined with Nicolai.");
		await user.click(within(toast).getByRole("button", { name: "Undo" }));
		await vi.waitFor(() =>
			expect(callsTo(harness.transport, "speakers.separate")).toEqual([
				{ method: "speakers.separate", params: { speakerID: UNNAMED } },
			]),
		);
	});

	it("says so in a toast when Undo fails", async () => {
		vi.spyOn(console, "error").mockImplementation(() => {});
		const user = userEvent.setup();
		const harness = await createBridgeHarness(
			"",
			{},
			{
				"speakers.select": refusingSelect,
				"speakers.separate": new Error("This voice is no longer combined."),
			},
		);
		const { speakers, transcript } = await detail("speakers-split");
		renderWithBridge(
			<>
				<SpeakersPopover speakers={speakers} transcript={transcript} />
				<ui.ToastRegion />
			</>,
			harness,
		);
		await user.click(screen.getByTestId("speakers-trigger"));
		await user.click(screen.getByRole("button", { name: "Same person" }));
		await user.click(await screen.findByRole("button", { name: "Undo" }));
		await vi.waitFor(() =>
			expect(
				screen
					.getAllByTestId("toast")
					.find((toast) => toast.dataset.tone === "destructive"),
			).toHaveTextContent(
				"Speaker 4 is still combined with Nicolai.This voice is no longer combined.",
			),
		);
	});

	it("nests a voice combined into a person's under them, with its line, play and Separate again", async () => {
		const { user, harness } = await open();
		const nested = screen.getByTestId(`speakers-nested-${NICOLAI}`);
		const combined = screen.getByTestId(
			`speakers-popover-combined-${COMBINED}`,
		);
		expect(nested).toContainElement(combined);
		expect(combined).toHaveTextContent(
			// The keys, shown only while keyboard focus is on the row, are S and P.
			"Speaker 5CombinedSeparate againSP“Und die Nebenprojekte pausieren wir bis zur Q1-Planung.”",
		);
		await user.click(
			within(combined).getByRole("button", {
				name: "Play the voice sample of Speaker 5",
			}),
		);
		expect(callsTo(harness.transport, "speakers.play")).toEqual([
			{ method: "speakers.play", params: { speakerID: COMBINED } },
		]);
		await user.click(
			within(combined).getByRole("button", { name: /^Separate again/ }),
		);
		expect(callsTo(harness.transport, "speakers.separate")).toEqual([
			{ method: "speakers.separate", params: { speakerID: COMBINED } },
		]);
	});

	it("keeps a combined voice nested, with Separate again and what it reads as, under a voice whose name was removed", async () => {
		// Removed before this visit: the panel places rows as it opens.
		const { user, speakers, republish } = await render("");
		republish(
			speakers.map((speaker) =>
				speaker.id === NICOLAI
					? {
							...speaker,
							assignment: "unknown" as const,
							displayName: speaker.clusterLabel,
						}
					: speaker,
			),
		);
		await user.click(screen.getByTestId("speakers-trigger"));
		const nested = within(
			screen.getByTestId("speakers-section-need"),
		).getByTestId(`speakers-nested-${NICOLAI}`);
		expect(
			screen.queryByTestId("speakers-section-people"),
		).not.toContainElement(nested);
		const combined = within(nested).getByTestId(
			`speakers-popover-combined-${COMBINED}`,
		);
		expect(
			within(combined).getByRole("button", { name: /^Separate again/ }),
		).toBeVisible();
		const root = speakers.find((speaker) => speaker.id === NICOLAI);
		expect(
			within(combined).getByTestId(`speakers-popover-reads-as-${COMBINED}`),
		).toHaveTextContent(`Reads as ${root?.clusterLabel}`);
	});

	it("separates with S on the row, and says why in red, without Retry, when its paragraphs moved", async () => {
		vi.spyOn(console, "error").mockImplementation(() => {});
		const { user, harness } = await open("", {
			"speakers.separate": () => {
				throw new BridgeError(
					"speakers.separate",
					"stale",
					"Its paragraphs moved since. Nothing to separate.",
				);
			},
		});
		await vi.waitFor(() => expect(row(ANNA)).toHaveFocus());
		await user.keyboard("{ArrowDown}{ArrowDown}{ArrowDown}");
		expect(row(COMBINED)).toHaveFocus();
		await user.keyboard("s");
		const failure = await screen.findByTestId(
			`speakers-popover-failure-${COMBINED}`,
		);
		expect(failure).toHaveTextContent(
			"Its paragraphs moved since. Nothing to separate.",
		);
		expect(within(failure).queryByRole("button", { name: "Retry" })).toBeNull();
		expect(callsTo(harness.transport, "speakers.separate")).toHaveLength(1);
	});

	it("offers Retry when Separate again failed for a passing reason", async () => {
		vi.spyOn(console, "error").mockImplementation(() => {});
		const { user, harness } = await open("", {
			"speakers.separate": new Error("database is locked"),
		});
		await vi.waitFor(() => expect(row(ANNA)).toHaveFocus());
		await user.keyboard("{ArrowDown}{ArrowDown}{ArrowDown}");
		await user.keyboard("s");
		const failure = await screen.findByTestId(
			`speakers-popover-failure-${COMBINED}`,
		);
		expect(failure).toHaveTextContent("database is locked");
		await user.click(within(failure).getByRole("button", { name: "Retry" }));
		await vi.waitFor(() =>
			expect(callsTo(harness.transport, "speakers.separate")).toHaveLength(2),
		);
	});

	it("opens the field without the person on Someone else", async () => {
		const { user } = await open("speakers-split");
		await user.click(screen.getByRole("button", { name: "Someone else" }));
		await screen.findAllByRole("option");
		expect(screen.queryByRole("option", { name: /Nicolai/ })).toBeNull();
	});

	it("keeps a row confirmed during the visit in place, open to correct, never confirmed twice", async () => {
		const { user, harness, speakers, republish } = await open();
		await vi.waitFor(() => expect(row(ANNA)).toHaveFocus());
		await user.keyboard("y");
		await vi.waitFor(() =>
			expect(
				screen.queryByTestId(`speakers-popover-saving-${ANNA}`),
			).toBeNull(),
		);
		republish(
			speakers.map((speaker) =>
				speaker.id === ANNA ? named(speaker, "Anna") : speaker,
			),
		);
		const check = screen.getByTestId("speakers-section-check");
		expect(check).toContainElement(row(ANNA));
		expect(within(row(ANNA)).getByLabelText("Named")).toBeVisible();
		expect(screen.queryByTestId(`speakers-popover-yes-${ANNA}`)).toBeNull();
		row(ANNA).focus();
		await user.keyboard("y");
		expect(callsTo(harness.transport, "speakers.select")).toHaveLength(1);
		await user.keyboard("{Enter}");
		expect(screen.getByTestId(`speaker-field-${ANNA}`)).toHaveFocus();
	});

	it("takes no pick on a row while it saves", async () => {
		const { user, harness } = await open("", NEVER);
		await vi.waitFor(() => expect(row(ANNA)).toHaveFocus());
		await user.keyboard("y");
		await screen.findByTestId(`speakers-popover-saving-${ANNA}`);
		await user.keyboard("y");
		await user.keyboard("{Enter}");
		await user.click(row(ANNA));
		expect(callsTo(harness.transport, "speakers.select")).toHaveLength(1);
		expect(screen.queryByRole("combobox")).toBeNull();
	});

	it("leaves the field for the next row with Tab and the previous with Shift+Tab, picking nothing", async () => {
		const { user, harness } = await open("speakers-none");
		await user.click(row(SPEAKER_1));
		await screen.findAllByRole("option");
		await user.keyboard("Ann");
		await user.tab();
		expect(screen.queryByRole("combobox")).toBeNull();
		expect(row(SPEAKER_2)).toHaveFocus();
		await user.keyboard("{Enter}");
		await user.tab({ shift: true });
		expect(row(SPEAKER_1)).toHaveFocus();
		expect(callsTo(harness.transport, "speakers.select")).toHaveLength(0);
	});

	it("walks every row with Tab and Shift+Tab, and leaves the panel past the last", async () => {
		const { user } = await open();
		await vi.waitFor(() => expect(row(ANNA)).toHaveFocus());
		const order = [ANNA, UNNAMED, NICOLAI, COMBINED, SPEAKER_1];
		for (const id of order.slice(1)) {
			await user.tab();
			expect(row(id)).toHaveFocus();
		}
		await user.tab({ shift: true });
		expect(row(COMBINED)).toHaveFocus();
		await user.tab();
		await user.tab();
		expect(
			screen.queryByTestId("speakers-panel")?.contains(document.activeElement),
		).not.toBe(true);
	});

	it("reopens a confirmed row with its name as the placeholder and the header Checked", async () => {
		const { user, speakers, republish } = await open();
		await vi.waitFor(() => expect(row(ANNA)).toHaveFocus());
		await user.keyboard("y");
		await vi.waitFor(() =>
			expect(
				screen.queryByTestId(`speakers-popover-saving-${ANNA}`),
			).toBeNull(),
		);
		republish(
			speakers.map((speaker) =>
				speaker.id === ANNA ? named(speaker, "Anna") : speaker,
			),
		);
		expect(
			within(screen.getByTestId("speakers-section-check")).getByRole("heading"),
		).toHaveTextContent("Checked");
		row(ANNA).focus();
		await user.keyboard("{Enter}");
		expect(screen.getByTestId(`speaker-field-${ANNA}`)).toHaveAttribute(
			"placeholder",
			"Anna",
		);
	});

	it("shows a row's keys, hidden until keyboard focus is on the row", async () => {
		await open();
		const yes = screen.getByTestId(`speakers-popover-yes-${ANNA}`);
		const key = within(yes).getByText("Y");
		expect(key.tagName).toBe("KBD");
		expect(key).toHaveAttribute("aria-hidden", "true");
		expect(key).toHaveClass("hidden", "group-focus-visible/row:inline-flex");
		expect(row(ANNA)).toHaveClass("group/row");
		expect(within(row(ANNA)).getByText("N").tagName).toBe("KBD");
		expect(within(row(ANNA)).getByText("P").tagName).toBe("KBD");
	});

	it("keeps a confirmed person's line on screen while they are renamed", async () => {
		const { user } = await open();
		expect(
			screen.queryByTestId(`speakers-popover-snippet-${NICOLAI}`),
		).toBeNull();
		await user.click(row(NICOLAI));
		expect(
			screen.getByTestId(`speakers-popover-snippet-${NICOLAI}`),
		).toBeVisible();
	});

	it("says You are Nicolai once Me is named", async () => {
		const toast = vi.spyOn(ui, "notify");
		const { user } = await open("speakers-none");
		await user.click(row(ME));
		await user.click(await screen.findByRole("option", { name: /Nicolai/ }));
		await vi.waitFor(() =>
			expect(toast).toHaveBeenCalledWith({ title: "You are Nicolai." }),
		);
	});

	it("does not reopen a voice named meanwhile when an earlier pick settles last", async () => {
		const { held, replies } = heldSelect();
		const { user, speakers, republish } = await open("speakers-none", replies);
		// Speaker 1's pick goes out and waits.
		await user.click(row(SPEAKER_1));
		await screen.findAllByRole("option");
		await user.keyboard("{Enter}");
		await screen.findByTestId(`speakers-popover-saving-${SPEAKER_1}`);
		// Speaker 2 is named and the host takes it first.
		await user.click(row(SPEAKER_2));
		await screen.findAllByRole("option");
		await user.keyboard("{Enter}");
		await screen.findByTestId(`speakers-popover-saving-${SPEAKER_2}`);
		held.get(SPEAKER_2)?.();
		await vi.waitFor(() =>
			expect(
				screen.queryByTestId(`speakers-popover-saving-${SPEAKER_2}`),
			).toBeNull(),
		);
		republish(
			speakers.map((speaker) =>
				speaker.id === SPEAKER_2 ? named(speaker, "Anna") : speaker,
			),
		);
		// Then Speaker 1's: nothing after it is unnamed now, so no field opens.
		held.get(SPEAKER_1)?.();
		await vi.waitFor(() =>
			expect(
				screen.queryByTestId(`speakers-popover-saving-${SPEAKER_1}`),
			).toBeNull(),
		);
		expect(screen.queryByRole("combobox")).toBeNull();
	});

	it("says No to a voice match at once: the voice moves to Need names, its field without Anna", async () => {
		const { user, harness, panel } = await open();
		await vi.waitFor(() => expect(row(ANNA)).toHaveFocus());
		await user.keyboard("n");
		expect(callsTo(harness.transport, "speakers.reject")).toEqual([
			{
				method: "speakers.reject",
				params: {
					speakerID: ANNA,
					personID: "00000000-0000-0000-0000-00000000000C",
				},
			},
		]);
		expect(
			within(screen.getByTestId("speakers-section-need")).getByTestId(
				`speakers-popover-name-${ANNA}`,
			),
		).toBeVisible();
		expect(screen.queryByTestId("speakers-section-check")).toBeNull();
		expect(row(ANNA)).not.toHaveTextContent("Sounds like");
		expect(screen.getByTestId(`speaker-field-${ANNA}`)).toHaveFocus();
		const options = await within(panel).findAllByRole("option");
		expect(options.map((option) => option.textContent)).not.toContain(
			"AAnnaSounds like · strong",
		);
	});

	it("offers the person said No to again once their name is typed", async () => {
		const { user, panel } = await open();
		await vi.waitFor(() => expect(row(ANNA)).toHaveFocus());
		await user.keyboard("n");
		await within(panel).findAllByRole("option");
		expect(within(panel).queryByRole("option", { name: /Anna/ })).toBeNull();
		await user.keyboard("Ann");
		const anna = await within(panel).findByRole("option", { name: /Anna/ });
		expect(anna).toBeVisible();
		// A person to pick again, not a voice match.
		expect(anna).not.toHaveTextContent("Sounds like");
	});

	it("brings a suggestion back with Retry when the No fails", async () => {
		vi.spyOn(console, "error").mockImplementation(() => {});
		const { user, harness } = await open("", {
			"speakers.reject": new Error("Speaker could not be changed: busy"),
		});
		await user.click(screen.getByTestId(`speakers-popover-no-${ANNA}`));
		const alert = await screen.findByTestId(`speakers-popover-failure-${ANNA}`);
		expect(alert).toHaveTextContent("Speaker could not be changed: busy");
		expect(row(ANNA)).toHaveTextContent("Sounds like Anna");
		expect(screen.getByTestId("speakers-section-check")).toBeVisible();
		await user.click(
			screen.getByTestId(`speakers-popover-failure-${ANNA}-retry`),
		);
		expect(callsTo(harness.transport, "speakers.reject")).toHaveLength(2);
	});

	it("marks a voice Not a person from its field, in place, with Restore", async () => {
		const toast = vi.spyOn(ui, "notify");
		const { user, harness, speakers, republish } = await open("speakers-none");
		await user.click(row(SPEAKER_1));
		await user.click(await screen.findByTestId("speaker-action-dismiss"));
		expect(callsTo(harness.transport, "speakers.dismiss")).toEqual([
			{ method: "speakers.dismiss", params: { speakerID: SPEAKER_1 } },
		]);
		await vi.waitFor(() =>
			expect(toast).toHaveBeenCalledWith({
				title: "Speaker 1 is not a person.",
			}),
		);
		republish(
			speakers.map((speaker) =>
				speaker.id === SPEAKER_1 ? { ...speaker, dismissed: true } : speaker,
			),
		);
		// Still where it was during this visit, drawn as it is now.
		expect(
			within(screen.getByTestId("speakers-section-need")).getByTestId(
				`speakers-popover-name-${SPEAKER_1}`,
			),
		).toHaveTextContent("Speaker 1Not a person");
		expect(screen.getByTestId("speakers-trigger")).toHaveTextContent(
			/^Name speakers$/,
		);
		await user.click(
			screen.getByTestId(`speakers-popover-restore-${SPEAKER_1}`),
		);
		expect(callsTo(harness.transport, "speakers.restore")).toEqual([
			{ method: "speakers.restore", params: { speakerID: SPEAKER_1 } },
		]);
	});

	it("draws a voice being dismissed muted, never as needing a name", async () => {
		const { user, speakers, republish } = await open("speakers-none", {
			"speakers.dismiss": () => new Promise(() => {}),
		});
		await user.click(row(SPEAKER_1));
		await user.click(await screen.findByTestId("speaker-action-dismiss"));
		// On its way: already not a person, saving, in its old place.
		expect(row(SPEAKER_1)).toHaveAccessibleName("Speaker 1, not a person");
		expect(row(SPEAKER_1)).toHaveTextContent(/^Speaker 1Not a personSaving/);
		expect(within(row(SPEAKER_1)).queryByText("?")).toBeNull();
		republish(
			speakers.map((speaker) =>
				speaker.id === SPEAKER_1 ? { ...speaker, dismissed: true } : speaker,
			),
		);
		// Saved: still in Need names for the visit, but no work there.
		expect(row(SPEAKER_1)).toHaveAccessibleName("Speaker 1, not a person");
		expect(within(row(SPEAKER_1)).queryByText("?")).toBeNull();
		expect(row(SPEAKER_1)).not.toHaveTextContent("Name");
	});

	it("lists voices that are not people last, under Not people, and restores with R", async () => {
		const { user, harness, panel } = await open("speakers-dismissed");
		expect(
			within(panel)
				.getAllByRole("heading")
				.map((heading) => heading.textContent),
		).toEqual(["Is this right?", "Speakers", "Not people"]);
		expect(screen.getByTestId("speakers-trigger")).toHaveTextContent(
			"Check 1 name",
		);
		expect(row(UNNAMED)).toHaveTextContent("Speaker 4Not a person");
		row(UNNAMED).focus();
		await user.keyboard("r");
		expect(callsTo(harness.transport, "speakers.restore")).toEqual([
			{ method: "speakers.restore", params: { speakerID: UNNAMED } },
		]);
	});

	it("quotes only what the clip plays; nearby words go unquoted", async () => {
		await open("speakers-context");
		const anna = screen.getByTestId(`speakers-popover-snippet-${ANNA}`);
		expect(anna.textContent).toMatch(/^Around the sample: \S/);
		expect(anna.textContent).not.toMatch(/[“”]/);
		// Without a clip it sits beside "No sample", with nothing to anchor.
		const unnamed = screen.getByTestId(`speakers-popover-snippet-${UNNAMED}`);
		expect(unnamed.textContent).not.toMatch(/Around the sample|[“”]/);
		expect(
			screen.getByTestId(`speakers-popover-play-${UNNAMED}-none`),
		).toHaveTextContent("No sample");
	});

	it("removes a confirmed name from the person's field", async () => {
		const toast = vi.spyOn(ui, "notify");
		const { user, harness } = await open();
		await user.click(row(NICOLAI));
		await user.click(await screen.findByTestId("speaker-action-clear"));
		expect(callsTo(harness.transport, "speakers.clear")).toEqual([
			{ method: "speakers.clear", params: { speakerID: NICOLAI } },
		]);
		await vi.waitFor(() =>
			expect(toast).toHaveBeenCalledWith({
				title: "Speaker 1 has no name now.",
			}),
		);
	});
});
