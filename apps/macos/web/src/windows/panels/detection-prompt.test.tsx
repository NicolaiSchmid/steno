import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { DetectionPrompt, parsePromptRequest } from "./detection-prompt";
import type { PanelShell } from "./panel-shell";

function fakeShell(): PanelShell & { calls: unknown[][] } {
	const calls: unknown[][] = [];
	return {
		calls,
		call: async (action, params) => {
			calls.push([action, params]);
		},
	};
}

describe("parsePromptRequest", () => {
	it("reads the app and the seconds, with defaults", () => {
		expect(
			parsePromptRequest(new URLSearchParams("app=Zoom&seconds=45")),
		).toEqual({
			appName: "Zoom",
			seconds: 45,
		});
		expect(parsePromptRequest(new URLSearchParams())).toEqual({
			appName: "An app",
			seconds: 60,
		});
		expect(parsePromptRequest(new URLSearchParams("app=+&seconds=-3"))).toEqual(
			{
				appName: "An app",
				seconds: 60,
			},
		);
		expect(
			parsePromptRequest(new URLSearchParams("seconds=soon")).seconds,
		).toBe(60);
	});

	it("reads the shell's number for the prompt, when it is one", () => {
		expect(
			parsePromptRequest(new URLSearchParams("app=Zoom&seconds=45&raised=3")),
		).toEqual({ appName: "Zoom", seconds: 45, raised: 3 });
		for (const raised of ["", "0", "-1", "1.5", "two"]) {
			expect(
				parsePromptRequest(new URLSearchParams(`raised=${raised}`)).raised,
			).toBeUndefined();
		}
	});
});

describe("DetectionPrompt", () => {
	it("Record tells the shell which prompt it was, never the bridge", async () => {
		const harness = await createBridgeHarness();
		const shell = fakeShell();
		renderWithBridge(
			<DetectionPrompt
				request={{ appName: "Zoom", seconds: 60, raised: 4 }}
				shell={shell}
			/>,
			harness,
		);
		await userEvent.click(screen.getByTestId("prompt-record"));
		expect(shell.calls).toEqual([["recordFromPrompt", { raised: 4 }]]);
		expect(callsTo(harness.transport, "recording.start")).toEqual([]);
	});

	it.each([
		[{ appName: "Zoom", seconds: 60 }, undefined],
		[{ appName: "Zoom", seconds: 60, raised: 4 }, { raised: 4 }],
	])(
		"the X tells the shell (%o) and never the bridge",
		async (request, params) => {
			const harness = await createBridgeHarness();
			const shell = fakeShell();
			renderWithBridge(
				<DetectionPrompt request={request} shell={shell} />,
				harness,
			);
			await userEvent.click(screen.getByTestId("prompt-dismiss"));
			expect(shell.calls).toEqual([["dismissPrompt", params]]);
			expect(harness.transport.calls).toEqual([]);
		},
	);

	it("reports its size in device pixels", async () => {
		vi.stubGlobal(
			"ResizeObserver",
			class {
				observe() {}
				disconnect() {}
			},
		);
		vi.stubGlobal("devicePixelRatio", 1.25);
		const measure = vi
			.spyOn(HTMLElement.prototype, "getBoundingClientRect")
			.mockReturnValue(new DOMRect(0, 0, 460, 56));
		const harness = await createBridgeHarness();
		const shell = fakeShell();
		renderWithBridge(
			<DetectionPrompt
				request={{ appName: "Zoom", seconds: 60 }}
				shell={shell}
			/>,
			harness,
		);
		expect(shell.calls.filter(([action]) => action === "resize")).toEqual([
			["resize", { width: 575, height: 70 }],
		]);
		measure.mockRestore();
		vi.unstubAllGlobals();
	});
});
