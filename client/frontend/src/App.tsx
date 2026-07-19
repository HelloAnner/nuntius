/* App shell: SSE lifecycle, routing, responsive chrome. */
import { useEffect } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { ToastHost, useTheme } from "@nuntius/shared";
import { startEvents } from "./events";
import { useArchiveOutboxRunner } from "./archiveOutbox";
import { useRoute, useThemeStore } from "./stores";
import { NavRail, TabBar } from "./components";
import { OverviewPage } from "./pages/Overview";
import { ProjectsPage } from "./pages/Projects";
import { ProjectPage } from "./pages/Project";
import { ThreadPage } from "./pages/Thread";
import { ThreadsPage } from "./pages/Threads";
import { ApprovalsPage } from "./pages/Approvals";

function Boot() {
  const qc = useQueryClient();
  const theme = useThemeStore((s) => s.theme);
  useTheme(theme);
  useArchiveOutboxRunner();

  useEffect(() => startEvents(qc), [qc]);

  return (
    <div className="app-shell">
      <NavRail />
      <div className="app-main">
        <RouterView />
        <TabBar />
      </div>
    </div>
  );
}

function RouterView() {
  const route = useRoute((s) => s.route);
  switch (route.name) {
    case "overview":
      return <OverviewPage />;
    case "projects":
      return <ProjectsPage />;
    case "threads":
      return <ThreadsPage />;
    case "approvals":
      return <ApprovalsPage />;
    case "project":
      return <ProjectPage key={route.projectId} projectId={route.projectId} />;
    case "thread":
      return (
        <ThreadPage
          key={route.threadId}
          projectId={route.projectId}
          threadId={route.threadId}
        />
      );
  }
}

export function App() {
  return (
    <ToastHost>
      <Boot />
    </ToastHost>
  );
}
