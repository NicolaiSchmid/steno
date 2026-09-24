import Constants from "expo-constants";

/**
 * Typed access to public runtime config. Build-coupled values (`appVariant`)
 * come from `app.config.ts` → `extra`; runtime endpoints come straight from
 * `process.env.EXPO_PUBLIC_*` (Metro inlines them into the JS bundle). The
 * latter are deliberately NOT in `extra` so they stay out of the native
 * fingerprint and can be changed via an OTA update without a rebuild.
 *
 * IMPORTANT: this must NOT throw at module load. In a release build a thrown
 * error here has no red-box overlay — the app just hard-crashes on open with
 * no clue why. Instead we collect the missing keys into `missingEnv`, and the
 * app renders a readable "missing config" screen (see StartupGate).
 *
 * The recorder has no required endpoints today: it talks only to the paired
 * Mac on the local network. `readRequired` stays so the first one is added
 * the right way, and so the env-contract CI gate (see mobile-cd.yml) can be
 * restored by pattern-matching on it.
 */

type RawExtra = {
	appVariant?: string;
};

const extra = (Constants.expoConfig?.extra ?? {}) as RawExtra;

/** Coerce a config value to a usable string, else null. `expoConfig.extra` can
 *  surface an unset nested value as `{}` (not null), which then defeats `??`
 *  fallbacks downstream — so accept only non-empty strings. */
function asString(value: unknown): string | null {
	return typeof value === "string" && value.length > 0 ? value : null;
}

/** Names of required EXPO_PUBLIC_* values that are missing at runtime. */
export const missingEnv: string[] = [];

export function readRequired(
	value: string | null | undefined,
	name: string,
): string {
	if (!value) {
		missingEnv.push(name);
		return "";
	}
	return value;
}

export const env = {
	// Build-coupled → read from native `extra`.
	appVariant: asString(extra.appVariant) ?? "development",
} as const;
