/* App shell: auth gate, SSE lifecycle, routing, responsive chrome. */
import { useEffect } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Spinner, ToastHost, useTheme } from "@nuntius/shared";
import { api, setCsrfProvider, ApiError } from "./api";
import { startEvents } from "./events";
import { useArchiveOutboxRunner } from "./archiveOutbox";
import { useRoute, useSession, useThemeStore } from "./stores";
import { NavRail, TabBar } from "./components";
import { AuthPage } from "./pages/Auth";
import { DevicesPage } from "./pages/Devices";
import { DevicePage } from "./pages/Device";
import { ProjectPage } from "./pages/Project";
import { ProjectsPage } from "./pages/Projects";
import { ThreadPage } from "./pages/Thread";
import { RecentsEntryRoute, RecentThreadRoute } from "./pages/RecentWorkspace";
import { ApprovalsPage } from "./pages/Approvals";
import { SettingsPage } from "./pages/Settings";

function Boot() {
  const qc = useQueryClient();
  const { session, setSession } = useSession();
  const theme = useThemeStore((s) => s.theme);
  const route = useRoute((state) => state.route);
  useTheme(theme);

  setCsrfProvider(() => useSession.getState().session?.csrfToken ?? null);

  const info = useQuery({ queryKey: ["info"], queryFn: api.info, staleTime: 60_000 });
  const sessionQuery = useQuery({
    queryKey: ["session"],
    queryFn: api.session,
    retry: false,
    staleTime: 30_000,
  });

  useEffect(() => {
    if (sessionQuery.data) setSession(sessionQuery.data);
    if (sessionQuery.error) setSession(null);
  }, [sessionQuery.data, sessionQuery.error, setSession]);

  const authed = Boolean(session);
  useArchiveOutboxRunner(authed);
  useEffect(() => {
    if (!authed) return;
    return startEvents(qc);
  }, [authed, qc]);

  if (info.isLoading || sessionQuery.isLoading) {
    return (
      <div className="boot-screen">
        <Spinner />
      </div>
    );
  }

  const unauthorized =
    sessionQuery.error instanceof ApiError && sessionQuery.error.status === 401;

  if (!session && (unauthorized || sessionQuery.error)) {
    return <AuthPage initialized={info.data?.initialized ?? true} />;
  }
  if (!session) {
    return (
      <div className="boot-screen">
        <Spinner />
      </div>
    );
  }

  return (
    <div className="app-shell server-console">
      <NavRail />
      <div className="app-main">
        <RouterView />
        {route.name === "thread" || route.name === "recentThread" ? null : <TabBar />}
      </div>
    </div>
  );
}

function RouterView() {
  const route = useRoute((s) => s.route);
  switch (route.name) {
    case "devices":
      return <DevicesPage />;
    case "recents":
      return <RecentsEntryRoute />;
    case "projects":
      return <ProjectsPage />;
    case "recentThread":
      return <RecentThreadRoute threadId={route.threadId} />;
    case "approvals":
      return <ApprovalsPage />;
    case "settings":
      return <SettingsPage />;
    case "device":
      return <DevicePage key={route.deviceId} deviceId={route.deviceId} />;
    case "project":
      return (
        <ProjectPage key={route.projectId} deviceId={route.deviceId} projectId={route.projectId} />
      );
    case "thread":
      return (
        <ThreadPage
          key={route.threadId}
          deviceId={route.deviceId}
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
