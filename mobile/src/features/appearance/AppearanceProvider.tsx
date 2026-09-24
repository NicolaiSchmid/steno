import * as SecureStore from "expo-secure-store";
import {
	createContext,
	useCallback,
	useContext,
	useEffect,
	useMemo,
	useState,
} from "react";
import { useColorScheme } from "react-native";
import { Uniwind } from "uniwind";
import {
	type AppearancePreference,
	parseAppearancePreference,
} from "./appearancePreference";

export type { AppearancePreference } from "./appearancePreference";

const STORAGE_KEY = "steno.appearance.v1";

type AppearanceContextValue = {
	preference: AppearancePreference;
	setPreference: (preference: AppearancePreference) => Promise<void>;
};

const AppearanceContext = createContext<AppearanceContextValue | null>(null);

function applyPreference(preference: AppearancePreference) {
	Uniwind.setTheme(preference);
}

export function AppearanceProvider({
	children,
}: {
	children: React.ReactNode;
}) {
	const [preference, setPreferenceState] =
		useState<AppearancePreference>("system");
	const [hydrated, setHydrated] = useState(false);

	useEffect(() => {
		let cancelled = false;
		void SecureStore.getItemAsync(STORAGE_KEY)
			.then((stored) => {
				if (cancelled) return;
				const next = parseAppearancePreference(stored);
				applyPreference(next);
				setPreferenceState(next);
			})
			.catch((error) =>
				console.warn("[appearance] Could not load preference", error),
			)
			.finally(() => {
				if (!cancelled) setHydrated(true);
			});
		return () => {
			cancelled = true;
		};
	}, []);

	const setPreference = useCallback(async (next: AppearancePreference) => {
		applyPreference(next);
		setPreferenceState(next);
		try {
			await SecureStore.setItemAsync(STORAGE_KEY, next);
		} catch (error) {
			console.warn("[appearance] Could not save preference", error);
		}
	}, []);

	const value = useMemo(
		() => ({ preference, setPreference }),
		[preference, setPreference],
	);
	if (!hydrated) return null;
	return (
		<AppearanceContext.Provider value={value}>
			{children}
		</AppearanceContext.Provider>
	);
}

export function useAppearancePreference() {
	const value = useContext(AppearanceContext);
	if (!value)
		throw new Error(
			"useAppearancePreference must be used inside AppearanceProvider",
		);
	return value;
}

export function useResolvedAppearance(): "light" | "dark" {
	const systemColorScheme = useColorScheme();
	const { preference } = useAppearancePreference();
	if (preference !== "system") return preference;
	return systemColorScheme === "dark" ? "dark" : "light";
}
