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
		expect(
			await selectSpeaker(harness.client, await unnamed(), NICOLAI),
		).toEqual({ kind: "committed" });
		expect(callsTo(harness.transport, "speakers.select")).toEqual([
			{
				method: "speakers.select",
				params: { speakerID: UNNAMED, option: NICOLAI, combine: false },
			},
		]);
	});

	it("combines a pick that would combine without asking, since Undo can take it back", async () => {
		const harness = await createBridgeHarness(
			"",
			{},
			{ "speakers.select": refusingSelect },
		);
		expect(
			await selectSpeaker(harness.client, await unnamed(), NICOLAI),
		).toEqual({ kind: "combined" });
		expect(
			callsTo(harness.transport, "speakers.select").map(
				(call) => (call.params as { combine: boolean }).combine,
			),
		).toEqual([false, true]);
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
		expect(callsTo(harness.transport, "speakers.select")).toHaveLength(1);
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
			"AAnnaSounds like · strong",
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
		await user.keyboard("{ArrowDown}");
		expect(list).toHaveAttribute("data-keys");
		await user.hover(second as HTMLElement);
		expect(list).not.toHaveAttribute("data-keys");
		await user.keyboard("{ArrowUp}");
		expect(list).toHaveAttribute("data-keys");
		expect(first).toHaveAttribute("data-highlighted");
	});

	it("draws no focus line before the arrows, and Enter still takes the first row", async () => {
		const { user, onSubmit, input } = await field();
		const [first] = await screen.findAllByRole("option");
		const list = screen.getByRole("listbox");
		expect(list).not.toHaveAttribute("data-keys");
		expect(first).toHaveAttribute("data-highlighted");
		expect(input).toHaveAttribute("aria-activedescendant", first?.id);
		await user.keyboard("{Enter}");
		expect(onSubmit).toHaveBeenCalledWith(
			expect.objectContaining({ label: "Anna" }),
			"",
		);
	});

	it("draws no focus line again for a new list until the arrows move", async () => {
		const { user, input } = await field();
		await screen.findAllByRole("option");
		const list = screen.getByRole("listbox");
		await user.keyboard("{ArrowDown}");
		expect(list).toHaveAttribute("data-keys");
		await user.type(input, "Ann");
		await vi.waitFor(() =>
			expect(screen.getByRole("option", { name: /Create/ })).toBeVisible(),
		);
		expect(list).not.toHaveAttribute("data-keys");
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

	describe("the voice's action", () => {
		async function withAction(
			speaker: Speaker,
			options: (params: unknown, recorded?: SpeakerOptionsReply) => unknown = (
				params,
				recorded,
			) => answerSpeakerOptions(recorded, params),
		) {
			const user = userEvent.setup();
			const recorded = (await loadFixtureReplies())["speakers.options"] as
				| SpeakerOptionsReply
				| undefined;
			const harness = await createBridgeHarness(
				"",
				{},
				{ "speakers.options": (params: unknown) => options(params, recorded) },
			);
			const onSubmit = vi.fn();
			const onAction = vi.fn();
			renderWithBridge(
				<SpeakerNameField
					onAction={onAction}
					onCancel={vi.fn()}
					onSubmit={onSubmit}
					speaker={speaker}
					speakers={(await meeting()).speakers}
				/>,
				harness,
			);
			return {
				user,
				onSubmit,
				onAction,
				input: screen.getByTestId(`speaker-field-${speaker.id}`),
			};
		}

		it("closes the list, reached by the arrows but never highlighted on its own", async () => {
			const { user, onSubmit, onAction } = await withAction(await unnamed());
			const action = await screen.findByTestId("speaker-action-dismiss");
			expect(action).toHaveTextContent("Not a person");
			expect(action).toHaveAttribute("aria-selected", "false");
			await user.keyboard("{ArrowDown}{ArrowDown}");
			expect(action).toHaveAttribute("aria-selected", "true");
			await user.keyboard("{Enter}");
			expect(onAction).toHaveBeenCalledWith("dismiss");
			expect(onSubmit).not.toHaveBeenCalled();
		});

		it("is not chosen by Enter when it is the only row", async () => {
			const { user, onAction } = await withAction(await unnamed(), () => ({
				options: [],
			}));
			const action = await screen.findByTestId("speaker-action-dismiss");
			expect(action).toHaveAttribute("aria-selected", "false");
			await user.keyboard("{Enter}");
			expect(onAction).not.toHaveBeenCalled();
		});

		it("goes while something is typed", async () => {
			const { user, input } = await withAction(await unnamed());
			await screen.findByTestId("speaker-action-dismiss");
			await user.type(input, "Ann");
			await vi.waitFor(() =>
				expect(screen.getByRole("option", { name: /Create/ })).toBeVisible(),
			);
			expect(screen.queryByTestId("speaker-action-dismiss")).toBeNull();
		});

		it("is Remove name for a confirmed voice", async () => {
			const speaker = await unnamed();
			await withAction({
				...speaker,
				assignment: "confirmed",
				displayName: "Lea",
				personID: "00000000-0000-0000-0000-0000000000AA",
			});
			expect(
				await screen.findByTestId("speaker-action-clear"),
			).toHaveTextContent("Remove name");
		});

		it("is Restore for a voice that is not a person", async () => {
			await withAction({ ...(await unnamed()), dismissed: true });
			expect(
				await screen.findByTestId("speaker-action-restore"),
			).toHaveTextContent("Restore");
		});

		it("is not offered for unnamed Me", async () => {
			await withAction({ ...(await unnamed()), clusterLabel: "Me" });
			await screen.findAllByRole("option");
			expect(screen.queryByTestId(/^speaker-action-/)).toBeNull();
		});
	});
});

describe("a paragraph's field", () => {
	async function paragraph(
		props: Partial<Parameters<typeof SpeakerNameField>[0]>,
	) {
		const user = userEvent.setup();
		const recorded = (await loadFixtureReplies())["speakers.options"] as
			| SpeakerOptionsReply
			| undefined;
		const harness = await createBridgeHarness(
			"",
			{},
			{
				"speakers.options": (params: unknown) =>
					answerSpeakerOptions(recorded, params),
			},
		);
		const onSubmit = vi.fn();
		renderWithBridge(
			<SpeakerNameField
				onCancel={vi.fn()}
				onSubmit={onSubmit}
				scope="turn"
				speaker={undefined}
				speakers={(await meeting()).speakers}
				{...props}
			/>,
			harness,
		);
		return { user, harness, onSubmit };
	}

	it("says nothing combines when one paragraph moves", async () => {
		await paragraph({ speaker: await unnamed() });
		const options = await screen.findAllByRole("option");
		expect(options.map((option) => option.textContent)).toEqual([
			"AAnnaSounds like · strong",
			"NNicolai",
		]);
		expect(screen.getByRole("combobox")).toHaveAccessibleName(
			"Name for this paragraph",
		);
	});

	it("lists the people named here for a turn without a voice, asking no voice", async () => {
		const { harness } = await paragraph({
			optionsSpeakerID: UNNAMED,
		});
		const input = screen.getByTestId("speaker-field-turn");
		expect(input).toHaveAttribute("placeholder", "Who said this?");
		const options = await screen.findAllByRole("option");
		// No voice's options: Anna, only suggested for Speaker 3, is not
		// named here, and nothing says "Sounds like".
		expect(options.map((option) => option.textContent)).toEqual([
			"NNicolai",
			"JJérôme",
		]);
		expect(callsTo(harness.transport, "speakers.options")).toHaveLength(0);
	});

	it("suggests the invitees nobody is yet before anything is typed, for a turn without a voice", async () => {
		const detail = await meeting();
		expect(detail.invitees).toEqual([
			{ kind: "create", label: "Lena", reason: "invited" },
		]);
		const { user, harness, onSubmit } = await paragraph({
			invitees: detail.invitees,
			optionsSpeakerID: UNNAMED,
		});
		const options = await screen.findAllByRole("option");
		expect(options.map((option) => option.textContent)).toEqual([
			"Create “Lena”Invited",
			"NNicolai",
			"JJérôme",
		]);
		expect(
			screen
				.getAllByRole("group")
				.map((group) => group.getAttribute("aria-label")),
		).toEqual(["Suggested", "Also speaking here"]);
		expect(callsTo(harness.transport, "speakers.options")).toHaveLength(0);
		await user.keyboard("{Enter}");
		expect(onSubmit).toHaveBeenCalledWith(
			{ kind: "create", label: "Lena", reason: "invited" },
			"",
		);
	});

	it("searches through another voice once typed, without its evidence or its Noes", async () => {
		const user = userEvent.setup();
		const recorded = (await loadFixtureReplies())["speakers.options"] as
			| SpeakerOptionsReply
			| undefined;
		const anna = recorded?.options.find((option) => option.label === "Anna");
		const harness = await createBridgeHarness(
			"",
			{},
			{
				// Speaker 4 said No to Anna: the hosts leave her out until typed.
				"speakers.options": (params: unknown) =>
					answerSpeakerOptions(
						recorded,
						params,
						"",
						new Set([`${UNNAMED} ${anna?.personID}`]),
					),
			},
		);
		renderWithBridge(
			<SpeakerNameField
				onCancel={vi.fn()}
				onSubmit={vi.fn()}
				optionsSpeakerID={UNNAMED}
				scope="turn"
				speaker={undefined}
				speakers={(await meeting()).speakers}
			/>,
			harness,
		);
		await screen.findAllByRole("option");
		await user.type(screen.getByTestId("speaker-field-turn"), "Ann");
		const found = await screen.findByRole("option", { name: /^Anna/ });
		// A plain person: no "Sounds like" from Speaker 4.
		expect(found.textContent).toBe("AAnna");
		expect(
			callsTo(harness.transport, "speakers.options").at(-1)?.params,
		).toEqual({ speakerID: UNNAMED, query: "Ann" });
	});

	it("offers only Create when the meeting has no voice to ask", async () => {
		const { user, harness, onSubmit } = await paragraph({});
		await user.type(screen.getByTestId("speaker-field-turn"), "Ann");
		const create = await screen.findByRole("option", { name: "Create “Ann”" });
		expect(create).toHaveAttribute("aria-selected", "true");
		await user.keyboard("{Enter}");
		expect(onSubmit).toHaveBeenCalledWith(
			{ kind: "create", label: "Ann" },
			"Ann",
		);
		expect(callsTo(harness.transport, "speakers.options")).toHaveLength(0);
	});
});
