import React from "react";
import ReactDOM from "react-dom/client";
import { BrowserRouter, Route, Routes } from "react-router-dom";
import { AppLayout } from "@/components/app-layout";
import Overview from "@/pages/overview";
import Profiles from "@/pages/profiles";
import Devices from "@/pages/devices";
import GreaseAi from "@/pages/grease-ai";
import SettingsPage from "@/pages/settings";
import "./index.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <BrowserRouter>
      <Routes>
        <Route element={<AppLayout />}>
          <Route index element={<Overview />} />
          <Route path="profiles" element={<Profiles />} />
          <Route path="devices" element={<Devices />} />
          <Route path="grease-ai" element={<GreaseAi />} />
          <Route path="settings" element={<SettingsPage />} />
        </Route>
      </Routes>
    </BrowserRouter>
  </React.StrictMode>,
);
