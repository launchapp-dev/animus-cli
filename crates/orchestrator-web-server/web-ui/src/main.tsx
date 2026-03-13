import React from "react";
import ReactDOM from "react-dom/client";

import { AppRouterProvider } from "./app/router";
import "./index.css";
import "./styles.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <AppRouterProvider />
  </React.StrictMode>,
);
