import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { PhoneSettingsSnapshot } from "@/bridge/contract";
import { loadFixtureSnapshots } from "@/bridge/mock-transport";
import { callsTo, createBridgeHarness, renderWithBridge } from "@/test/bridge";
import { PhoneSection } from "./iphone-section";

const PHONE_ID = "00000000-0000-0000-0000-000000000028";

describe("PhoneSection", () => {
	it("lists the paired phone and removes it through the host", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness();
		renderWithBridge(<PhoneSection />, harness);
		const row = screen.getByTestId(`device-${PHONE_ID}`);
		expect(row).toHaveTextContent("Nicolai's iPhone");
		expect(row).toHaveTextContent(/Paired .* · last seen /);
		expect(screen.queryByTestId("pairing-card")).not.toBeInTheDocument();
		await user.click(screen.getByTestId(`revoke-${PHONE_ID}`));
		expect(callsTo(harness.transport, "settings.iphone.revoke")).toEqual([
			{ method: "settings.iphone.revoke", params: { deviceID: PHONE_ID } },
		]);
		await user.click(screen.getByTestId("begin-pairing"));
		expect(
			callsTo(harness.transport, "settings.iphone.beginPairing"),
		).toHaveLength(1);
	});

	it("shows the pairing code with its expiry and a transfer arriving", async () => {
		const user = userEvent.setup();
		const harness = await createBridgeHarness("scenario=pairing");
		renderWithBridge(<PhoneSection />, harness);
		const code = screen.getByRole("img", {
			name: "Pairing code for the Steno iPhone app",
		});
		expect(code).toHaveAttribute(
			"src",
			expect.stringMatching(/^data:image\/png;base64,/),
		);
		expect(screen.getByTestId("pairing-expiry")).toHaveTextContent(
			"It expires in 4 minutes.",
		);
		expect(screen.getByTestId("begin-pairing")).toBeDisabled();
		expect(
			screen.getByText("Receiving from Nicolai's iPhone…"),
		).toBeInTheDocument();
		expect(screen.getByRole("progressbar")).toHaveAttribute(
			"aria-valuenow",
			"50",
		);
		await user.click(screen.getByTestId("cancel-pairing"));
		expect(
			callsTo(harness.transport, "settings.iphone.cancelPairing"),
		).toHaveLength(1);
	});

	it("says when pairing is unavailable", async () => {
		const harness = await createBridgeHarness("scenario=phone-unavailable");
		renderWithBridge(<PhoneSection />, harness);
		expect(screen.getByTestId("phone-unavailable")).toHaveTextContent(
			"Pairing is unavailable right now.",
		);
		expect(screen.queryByTestId("begin-pairing")).not.toBeInTheDocument();
	});

	it("shows the listener's failure with its details", async () => {
		const user = userEvent.setup();
		const phone = (await loadFixtureSnapshots())[
			"settings.iphone"
		] as PhoneSettingsSnapshot;
		const harness = await createBridgeHarness("", {
			"settings.iphone": {
				...phone,
				listener: { state: "failed", failure: "Address already in use." },
			} satisfies PhoneSettingsSnapshot,
		});
		renderWithBridge(<PhoneSection />, harness);
		expect(screen.getByTestId("listener-failed")).toHaveTextContent(
			"Steno cannot receive recordings right now.",
		);
		await user.click(screen.getByText("Details"));
		expect(screen.getByText("Address already in use.")).toBeInTheDocument();
	});
});
