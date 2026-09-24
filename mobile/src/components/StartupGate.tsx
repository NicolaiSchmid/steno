import * as SplashScreen from "expo-splash-screen";
import { Component, type ErrorInfo, type ReactNode, useEffect } from "react";
import { ScrollView, StyleSheet, Text } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";

import { PressableScale } from "@/components/PressableScale";
import { missingEnv } from "@/lib/env";

/**
 * Makes startup failures VISIBLE on-device. Release builds have no red-box
 * overlay, so a thrown error or missing config is otherwise just a silent
 * crash on open. This renders a readable, selectable diagnostic instead:
 *  - missing EXPO_PUBLIC_* config → lists exactly what's absent
 *  - any other render/startup error → shows message + stack (via ErrorBoundary)
 *
 * Deliberately English-only and hard-coloured: it sits ABOVE the theme
 * provider, so it can render when that is the thing that broke.
 */
function DiagnosticScreen({
	title,
	detail,
	onRetry,
}: {
	title: string;
	detail: string;
	onRetry?: () => void;
}) {
	// The splash is held open in index.ts and normally released by
	// RootNavigator — which never commits when startup fails above it. Without
	// this the splash would sit on top of the diagnostic forever.
	useEffect(() => {
		void SplashScreen.hideAsync();
	}, []);

	return (
		<SafeAreaView style={styles.root}>
			<ScrollView contentContainerStyle={styles.content}>
				<Text style={styles.title}>{title}</Text>
				{onRetry ? (
					<PressableScale
						accessibilityRole="button"
						onPress={onRetry}
						style={styles.retryButton}
					>
						<Text style={styles.retryLabel}>Try again</Text>
					</PressableScale>
				) : null}
				<Text selectable style={styles.detail}>
					{detail}
				</Text>
			</ScrollView>
		</SafeAreaView>
	);
}

class ErrorBoundary extends Component<
	{ children: ReactNode },
	{ error: Error | null }
> {
	state = { error: null as Error | null };

	static getDerivedStateFromError(error: Error) {
		return { error };
	}

	componentDidCatch(error: Error, info: ErrorInfo) {
		console.error("[startup] uncaught error", error, info.componentStack);
	}

	// Clearing the error re-renders children from scratch. If the crash cause
	// was transient, the retry recovers in place instead of demanding a manual
	// app relaunch.
	reset = () => {
		this.setState({ error: null });
	};

	render() {
		const { error } = this.state;
		if (error) {
			return (
				<DiagnosticScreen
					detail={`${error.message}\n\n${error.stack ?? ""}`}
					onRetry={this.reset}
					title="Something crashed at startup"
				/>
			);
		}
		return this.props.children;
	}
}

export function StartupGate({ children }: { children: ReactNode }) {
	if (missingEnv.length > 0) {
		return (
			<DiagnosticScreen
				detail={`These EXPO_PUBLIC_* values weren't baked into this build:\n\n• ${missingEnv.join(
					"\n• ",
				)}\n\nSet them as EAS environment variables (dev/preview/production) or in mobile/.env for local builds, then rebuild.`}
				title="Missing configuration"
			/>
		);
	}
	return <ErrorBoundary>{children}</ErrorBoundary>;
}

const styles = StyleSheet.create({
	root: { flex: 1, backgroundColor: "#000000" },
	content: { padding: 24, gap: 16, paddingTop: 60 },
	title: { color: "#ffffff", fontSize: 20, fontWeight: "700" },
	detail: {
		color: "#d4d4d4",
		fontSize: 13,
		lineHeight: 19,
		fontFamily: "Menlo",
	},
	retryButton: {
		alignSelf: "flex-start",
		backgroundColor: "#ffffff",
		borderRadius: 10,
		paddingHorizontal: 18,
		paddingVertical: 10,
	},
	retryLabel: { color: "#000000", fontSize: 15, fontWeight: "600" },
});
