import React from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter } from "react-router-dom";
import "@/index.css";
import { App } from "@/App";
import { CogwheelProvider } from "@/data/provider";
import { ErrorBoundary } from "@/components/layout/error-boundary";

const container = document.getElementById("root");
if (!container) throw new Error("#root is missing from index.html");

createRoot(container).render(
  <React.StrictMode>
    <ErrorBoundary>
      <BrowserRouter>
        <CogwheelProvider>
          <App />
        </CogwheelProvider>
      </BrowserRouter>
    </ErrorBoundary>
  </React.StrictMode>,
);
