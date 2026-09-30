import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./app";
import "./theme.css";

const container = document.getElementById("root");
if (!container) {
	throw new Error("index.html has no #root");
}

createRoot(container).render(
	<StrictMode>
		<App />
	</StrictMode>,
);
