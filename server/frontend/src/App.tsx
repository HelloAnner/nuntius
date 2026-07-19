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
import { ThreadPage } from "./pages/Thread";
import { RecentsPage } from "./pages/Recents";
import { ApprovalsPage } from "./pages/Approvals";
import { SettingsPage } from "./pages/Settings";

function Boot() {
  const qc = useQueryClient();
  const { session, setSession } = useSession();
  const theme = useThemeStore((s) => s.theme);
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
      <div style={{ minHeight: "100dvh", display: "grid", placeItems: "center" }}>
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
      <div style={{ minHeight: "100dvh", display: "grid", placeItems: "center" }}>
        <Spinner />
      </div>
    );
  }

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
    case "devices":
      return <DevicesPage />;
    case "recents":
      return <RecentsPage />;
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

function RecentThreadRoute({ threadId }: { threadId: string }) {
  const navigate = useRoute((state) => state.navigate);
  const threads = useQuery({ queryKey: ["allThreads"], queryFn: () => api.allThreads() });
  const thread = threads.data?.find((item) => item.id === threadId);

  useEffect(() => {
    if (threads.isSuccess && !thread) navigate({ name: "recents" }, { replace: true });
  }, [navigate, thread, threads.isSuccess]);

  if (!thread) {
    return (
      <div className="page" style={{ display: "grid", placeItems: "center" }}>
        <Spinner />
      </div>
    );
  }

  return (
    <ThreadPage
      navigationContext="recents"
      deviceId={thread.deviceId}
      projectId={thread.projectId}
      threadId={thread.id}
    />
  );
}

export function App() {
  return (
    <ToastHost>
      <Boot />
    </ToastHost>
  );
}
