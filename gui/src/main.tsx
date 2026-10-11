import { initializeBudgets } from "./limits/policy";
import { initializeDiagnostics } from "./local-telemetry";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { readOpenRoute, readScreenRoute } from "./surface/open-route";
import { followDetachedRoutes } from "./surface/detached-routing";
import { Boundary } from "./Boundary";
import { defaults } from "./settings";
import { initializeDesktopPreferences } from "./desktop-preferences";
import { initializeDesktopClient } from "./client-identity";

import "./base.css";

async function start() {
  await initializeDesktopClient();
  await initializeBudgets();
  initializeDiagnostics();
  const {followPresentations}=await import("./presentation/transport");
  const [{Gallery,readGalleryRoute},{SurfaceApp},{OpenWindow},{ScreenWindow}] = await Promise.all([import("./surface/Gallery"),import("./surface/SurfaceApp"),import("./surface/OpenWindow"),import("./surface/ScreenWindow")]);
  // Resolve personal preferences before the first paint.
  await initializeDesktopPreferences(defaults);

  const root = document.getElementById("root");
  if (root === null) {
    throw new Error("index.html has no #root to draw into");
  }

  // Synthetic design galleries remain addressable independently of saved preferences.
  const gallery = readGalleryRoute(window.location.hash);
  // A result opened in its own window or tab is this same client, told to draw one node.
  const opened = readOpenRoute(window.location.hash);
  // A screen summoned into a window of its own: the graph, or a settings section.
  const summoned = readScreenRoute(window.location.hash);

  // The data home's presentation entries follow the engine's directory for as long as the page lives.
  if (!gallery) void followPresentations(new AbortController().signal);

  const mounted = createRoot(root);
  const draw = (hash: string) => {
    const opened = readOpenRoute(hash);
    const summoned = readScreenRoute(hash);
    mounted.render(
      <StrictMode>
        <Boundary>
          {gallery ? <Gallery route={gallery} />
            : opened ? <OpenWindow key={`${opened.workspace}:${opened.node ?? opened.cell}:${opened.peek ?? "open"}:${opened.tab}`} route={opened} />
            : summoned ? <ScreenWindow key={`${summoned.workspace}:${summoned.screen === "stale" ? "graph" : summoned.screen}`} route={summoned} />
            : <SurfaceApp />}
        </Boundary>
      </StrictMode>,
    );
  };
  draw(window.location.hash);
  if (opened || summoned) followDetachedRoutes(window.location.hash, draw);
}
function StartupFailure({error}:{readonly error:unknown}):never { throw error instanceof Error?error:new Error(String(error)); }
void start().catch(error=>{
  const root=document.getElementById("root");
  if(root)createRoot(root).render(<Boundary><StartupFailure error={error}/></Boundary>);
});
