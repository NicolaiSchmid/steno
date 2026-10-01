import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { normaliseTag, TagEditor } from "./tag-editor";

describe("normaliseTag", () => {
	it("trims, drops leading hashes and lower-cases", () => {
		expect(normaliseTag("  #Strategie ")).toBe("strategie");
		expect(normaliseTag("##Q4")).toBe("q4");
		expect(normaliseTag("   ")).toBe("");
	});
});

describe("TagEditor", () => {
	it("reads Add tag with no tags and Edit tags once there are some", async () => {
		const harness = await createBridgeHarness();
		const { rerender } = renderWithBridge(<TagEditor tags={[]} />, harness);
		expect(screen.getByTestId("edit-tags")).toHaveTextContent("Add tag");
		expect(screen.getByTestId("edit-tags")).not.toHaveAttribute("aria-label");
		rerender(<TagEditor tags={["q4"]} />);
		expect(screen.getByRole("button", { name: "Edit tags" })).toHaveTextContent(
			"",
		);
	});

	it("lists each tag as a removable button and sends the rest", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<TagEditor tags={["strategie", "q4"]} />, harness);
		await user.click(screen.getByTestId("edit-tags"));
		const remove = await screen.findByRole("button", {
			name: "Remove tag q4",
		});
		expect(remove).toBe(screen.getByTestId("remove-tag-q4"));
		expect(remove).toHaveTextContent("#q4");
		expect(screen.getByTestId("remove-tag-strategie")).toBeInTheDocument();
		await user.click(remove);
		expect(callsTo(harness.transport, "meeting.setTags")).toEqual([
			{ method: "meeting.setTags", params: { tags: ["strategie"] } },
		]);
	});

	it("adds the normalised draft on Return and ignores duplicates", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<TagEditor tags={["q4"]} />, harness);
		await user.click(screen.getByTestId("edit-tags"));
		const field = await screen.findByTestId("tags-field");
		await user.type(field, " #Investors{Enter}");
		expect(callsTo(harness.transport, "meeting.setTags")).toEqual([
			{ method: "meeting.setTags", params: { tags: ["q4", "investors"] } },
		]);
		expect(field).toHaveValue("");
		await user.type(field, "Q4{Enter}");
		expect(callsTo(harness.transport, "meeting.setTags")).toHaveLength(1);
		await user.type(field, "{Enter}");
		expect(callsTo(harness.transport, "meeting.setTags")).toHaveLength(1);
	});
});
