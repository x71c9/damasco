import {
  useEffect,
  useRef,
  useState,
  type KeyboardEvent,
  type PointerEvent,
} from "react";
import {
  agent,
  can,
  fetchBuild,
  fetchInstallation,
  siblingUrl,
  type AgentAccount,
  type Installation,
  type Me,
} from "./api";
import { Chat } from "./components/Chat";
import { ConnectAssistant } from "./components/ConnectAssistant";
import { Header, type Device, type MobileView } from "./components/Header";
import { NavRail, type Section } from "./components/NavRail";
import { Preview } from "./components/Preview";
import { ActivityPage } from "./activity/ActivityPage";
import { PeoplePage } from "./people/PeoplePage";
import { MediaPage } from "./media/MediaPage";
import { PageEditor } from "./pages/PageEditor";
import { PagesPage } from "./pages/PagesPage";
import { currentTheme, setTheme, type Theme } from "./theme";
import "./Workspace.css";

const CHAT_MIN = 360;
/** The chat takes at most this share of the space it shares with the
 * preview. */
const CHAT_SHARE = 0.75;
const CHAT_WIDTH_KEY = "axioma.chat.width";

/** The chat's width this browser remembers, if any. */
function storedChatWidth(): number {
  try {
    const saved = Number(localStorage.getItem(CHAT_WIDTH_KEY));
    return Number.isFinite(saved) && saved >= CHAT_MIN ? saved : 420;
  } catch {
    return 420;
  }
}

const paths: Record<Section, string> = {
  website: "/",
  activity: "/activity",
  people: "/people",
  pages: "/pages",
  media: "/media",
};

function sectionOf(pathname: string): Section {
  if (pathname === "/people") return "people";
  if (pathname === "/activity") return "activity";
  if (pathname === "/pages" || pathname.startsWith("/pages/")) return "pages";
  if (pathname === "/media") return "media";
  return "website";
}

/** The signed-in panel: header, sections, preview and chat. */
export function Workspace({
  me,
  onSignedOut,
}: {
  me: Me;
  onSignedOut: () => void;
}) {
  const [installation, setInstallation] = useState<Installation | null>(null);
  const [device, setDevice] = useState<Device>("computer");
  const [reloadKey, setReloadKey] = useState(0);
  const [mobileView, setMobileView] = useState<MobileView>("chat");
  const [chatWidth, setChatWidth] = useState(storedChatWidth);
  // The widest the chat may be: a share of the space around it, which
  // changes with the window.
  const [chatMax, setChatMax] = useState(720);
  const main = useRef<HTMLElement>(null);
  useEffect(() => {
    const element = main.current;
    if (!element) return;
    const measure = () =>
      setChatMax(Math.max(CHAT_MIN, Math.floor(element.clientWidth * CHAT_SHARE)));
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  });
  const clamp = (width: number) => Math.min(chatMax, Math.max(CHAT_MIN, width));
  // The width the person chose, kept by this browser; a narrower window
  // shows less without forgetting it.
  useEffect(() => {
    try {
      localStorage.setItem(CHAT_WIDTH_KEY, String(Math.round(chatWidth)));
    } catch {
      // Not remembered: the width lasts until the page is closed.
    }
  }, [chatWidth]);
  const shownChatWidth = Math.min(chatWidth, chatMax);
  const [theme, setThemeState] = useState<Theme>(currentTheme);
  const toggleTheme = () => {
    const next = theme === "dark" ? "light" : "dark";
    setTheme(next);
    setThemeState(next);
  };
  const drag = useRef<{ x: number; width: number } | null>(null);
  const [section, setSection] = useState<Section>(() =>
    sectionOf(window.location.pathname),
  );

  // The page open in the editor, from the address /pages/<id>.
  const pageOf = (pathname: string) =>
    pathname.startsWith("/pages/") ? decodeURIComponent(pathname.slice(7)) : null;
  const [pageId, setPageId] = useState<string | null>(() =>
    pageOf(window.location.pathname),
  );
  const openPage = (id: string | null) => {
    const path = id ? `/pages/${encodeURIComponent(id)}` : "/pages";
    if (window.location.pathname !== path) window.history.pushState(null, "", path);
    setSection("pages");
    setPageId(id);
  };

  // Each section has its own address, so reload and the back button work.
  useEffect(() => {
    const onPop = () => {
      setSection(sectionOf(window.location.pathname));
      setPageId(pageOf(window.location.pathname));
    };
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);
  const navigate = (next: Section) => {
    if (next !== section || pageId) window.history.pushState(null, "", paths[next]);
    setSection(next);
    setPageId(null);
  };
  // A section the person may not open falls back to the website.
  const shown: Section =
    section === "website" ||
    (section === "pages" || section === "media"
      ? can(me, "content")
      : can(me, section))
      ? section
      : "website";

  useEffect(() => {
    fetchInstallation().then(setInstallation, (error: unknown) =>
      console.error(error),
    );
  }, []);

  // The chat needs the assistant's account; only owner and administrators
  // use it.
  const chat = can(me, "chat", "write");
  const [account, setAccount] = useState<AgentAccount | null>(null);
  useEffect(() => {
    if (chat) agent.account().then(setAccount, () => {});
  }, [chat]);

  // The preview reloads by itself when a new build of the site is ready.
  // Its address names the build, so the browser never shows a copy of an
  // older one from its cache.
  const [release, setRelease] = useState<string | null | undefined>(
    undefined,
  );
  useEffect(() => {
    const check = () =>
      fetchBuild().then(
        (status) => setRelease(status.release),
        () => setRelease((current) => current ?? null),
      );
    check();
    const timer = window.setInterval(check, 2000);
    return () => window.clearInterval(timer);
  }, []);

  // At the end of each answer the preview loads again, with the build that
  // includes the changes.
  const showLatestBuild = () => {
    fetchBuild().then(
      (status) => {
        setRelease(status.release);
        setReloadKey((key) => key + 1);
      },
      () => setReloadKey((key) => key + 1),
    );
  };

  // The separator sits on the left of the chat: dragging left widens it.
  const onPointerDown = (event: PointerEvent<HTMLDivElement>) => {
    event.currentTarget.setPointerCapture(event.pointerId);
    drag.current = { x: event.clientX, width: shownChatWidth };
  };
  const onPointerMove = (event: PointerEvent<HTMLDivElement>) => {
    if (drag.current)
      setChatWidth(clamp(drag.current.width + drag.current.x - event.clientX));
  };
  const onPointerUp = () => {
    drag.current = null;
  };
  const onSeparatorKey = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "ArrowLeft") setChatWidth(clamp(shownChatWidth + 20));
    if (event.key === "ArrowRight") setChatWidth(clamp(shownChatWidth - 20));
  };

  const siteName = installation?.site_hosts[0] ?? "Site";
  const previewUrl = installation
    ? siblingUrl(installation.preview_host)
    : null;

  return (
    <div className="app" data-mobile-view={mobileView} data-section={shown}>
      <Header
        section={shown}
        siteName={siteName}
        device={device}
        onDevice={setDevice}
        onReload={() => setReloadKey((key) => key + 1)}
        pending={0}
        mobileView={mobileView}
        onMobileView={setMobileView}
        theme={theme}
        onToggleTheme={toggleTheme}
      />
      <div className="app-body">
        <NavRail
          current={shown}
          onNavigate={navigate}
          me={me}
          onSignedOut={onSignedOut}
        />
        {/* One layout for every section: the section on the left, the chat
            on the right. The chat stays mounted across sections, so its
            conversation, terminal and stream go on without reloading. */}
        <main className="app-main" ref={main}>
          <div className="app-section">
            {shown === "people" ? (
              <PeoplePage me={me} />
            ) : shown === "pages" ? (
              pageId ? (
                <PageEditor
                  key={pageId}
                  id={pageId}
                  previewUrl={previewUrl}
                  onBack={() => openPage(null)}
                />
              ) : (
                <PagesPage onOpen={openPage} />
              )
            ) : shown === "media" ? (
              <MediaPage />
            ) : shown === "activity" ? (
              <ActivityPage />
            ) : (
            <Preview
              url={
                previewUrl
                  ? release === undefined
                    ? undefined
                    : release
                      ? `${previewUrl}?build=${release}`
                      : previewUrl
                  : null
              }
              device={device}
              reloadKey={reloadKey}
            />
            )}
          </div>
            {chat && (
              <>
                <div
                  className="app-separator"
                  role="separator"
                  aria-orientation="vertical"
                  aria-label="Drag to resize the chat"
                  aria-valuemin={CHAT_MIN}
                  aria-valuemax={chatMax}
                  aria-valuenow={shownChatWidth}
                  tabIndex={0}
                  onPointerDown={onPointerDown}
                  onPointerMove={onPointerMove}
                  onPointerUp={onPointerUp}
                  onKeyDown={onSeparatorKey}
                >
                  <span />
                </div>
                <div className="app-chat" style={{ width: shownChatWidth }}>
                  {account?.signed_in ? (
                    <Chat
                      me={me.id}
                      account={account}
                      onDisconnected={setAccount}
                      onTurnEnd={showLatestBuild}
                    />
                  ) : (
                    <ConnectAssistant
                      account={account}
                      onConnected={setAccount}
                    />
                  )}
                </div>
              </>
            )}
        </main>
      </div>
    </div>
  );
}
