import { Button } from "../../src/components/ui/button";

/**
 * Fixture for scripts/check-ui-restyle.test.ts: the first button is fine
 * (layout only), the second restyles the component and must be reported.
 */
export function RestyleFixture() {
	return (
		<div>
			<Button className="w-full justify-start" variant="primary">
				Layout only
			</Button>
			<Button className="w-full bg-destructive text-[11px] hover:opacity-50">
				Restyled
			</Button>
		</div>
	);
}
