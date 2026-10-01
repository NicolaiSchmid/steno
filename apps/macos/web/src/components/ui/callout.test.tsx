import { render, screen } from "@testing-library/react";
import { CircleAlertIcon } from "lucide-react";
import { describe, expect, it } from "vitest";
import { Button } from "./button";
import { Callout, calloutVariants } from "./callout";

const VARIANTS = [
	"default",
	"warning",
	"info",
	"live",
	"success",
	"destructive",
] as const;

describe("Callout", () => {
	it.each(VARIANTS)(
		"renders icon, title, description and actions as a status (%s)",
		(variant) => {
			render(
				<Callout
					actions={<Button size="xs">Fix it</Button>}
					data-testid="notice"
					description="Allow it in System Settings."
					icon={<CircleAlertIcon aria-hidden="true" data-testid="icon" />}
					title="Steno can't use the microphone."
					variant={variant}
				/>,
			);
			const notice = screen.getByRole("status");
			expect(notice).toBe(screen.getByTestId("notice"));
			expect(notice).toHaveTextContent(
				"Steno can't use the microphone. Allow it in System Settings.",
			);
			expect(screen.getByTestId("icon").parentElement).toHaveAttribute(
				"data-slot",
				"icon",
			);
			expect(screen.getByText("Allow it in System Settings.")).toHaveAttribute(
				"data-slot",
				"description",
			);
			expect(
				screen.getByRole("button", { name: "Fix it" }),
			).toBeInTheDocument();
		},
	);

	it("colours the surface per variant", () => {
		expect(calloutVariants({ variant: "warning" })).toContain(
			"bg-warning-surface",
		);
		expect(calloutVariants({ variant: "info" })).toContain("border-info/32");
		expect(calloutVariants({ variant: "live" })).toContain("border-primary/32");
		expect(calloutVariants({ variant: "destructive" })).toContain(
			"bg-destructive-surface",
		);
		expect(calloutVariants({ variant: "success" })).toContain(
			"bg-success-surface",
		);
		expect(calloutVariants({ variant: "default" })).toContain("bg-card");
		expect(calloutVariants({})).toContain("bg-card");
	});

	it("leaves out the description and actions when not given", () => {
		render(<Callout icon={<CircleAlertIcon />} title="Just the title" />);
		const notice = screen.getByRole("status");
		expect(notice).toHaveTextContent(/^Just the title$/);
		expect(notice.querySelector("[data-slot=description]")).toBeNull();
		expect(screen.queryByRole("button")).not.toBeInTheDocument();
	});

	it("uses the small type in the sidebar size", () => {
		expect(calloutVariants({ size: "sm" })).toContain("text-xs");
		expect(calloutVariants({ size: "md" })).toContain("text-sm");
	});
});
