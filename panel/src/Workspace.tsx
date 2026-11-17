import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type KeyboardEvent,
  type PointerEvent,
} from "react";
import {
  agent,
  can,
  components as componentsApi,
  media as mediaApi,
  pages as pagesApi,
  previewEntry,
  fetchBuild,
  fetchInstallation,
  siblingUrl,
  type AgentAccount,
  type Installation,
  type Me,
} from "./api";
import { ChangesProvider } from "./changes/changes";
import { NoticesProvider } from "./notices/Notices";
import { Chat } from "./components/Chat";
import { ConnectAssistant } from "./components/ConnectAssistant";
import { Header, type Device, type Environment, type MobileView } from "./components/Header";
import { NavRail, type Section } from "./components/NavRail";
import { settingsSections, type SettingsSection } from "./settings/sections";
import { SettingsPage } from "./settings/SettingsPage";
import { Preview } from "./components/Preview";
import { ActivityPage } from "./activity/ActivityPage";
import { PeoplePage } from "./people/PeoplePage";
import { MediaPage } from "./media/MediaPage";
import { PageEditor } from "./pages/PageEditor";
import { PagesPage } from "./pages/PagesPage";
import { ComponentsPage } from "./parts/ComponentsPage";
import { ComponentEditor } from "./parts/ComponentEditor";
import { PublishDialog } from "./publish/PublishDialog";
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
  activity: "/settings/activity",
  people: "/settings/people",
  pages: "/pages",
  components: "/components",
  media: "/media",
};

function sectionOf(pathname: string): Section {
  // Settings open on their first section; /people and /activity are the
  // addresses of before.
  if (pathname === "/settings" || pathname === "/settings/people" || pathname === "/people")
    return "people";
  if (pathname === "/settings/activity" || pathname === "/activity") return "activity";
  if (pathname === "/pages" || pathname.startsWith("/pages/")) return "pages";
  if (pathname === "/components" || pathname.startsWith("/components/")) return "components";
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
  const [publishing, setPublishing] = useState(false);
  // The chat forward: on a phone it takes the screen; its box takes the
  // cursor.
  const openChat = useCallback(() => {
    setMobileView("chat");
    window.setTimeout(() => document.getElementById("composer-input")?.focus(), 0);
  }, []);
  const [chosenEnvironment, setEnvironment] = useState<Environment>("preview");
  const [section, setSection] = useState<Section>(() =>
    sectionOf(window.location.pathname),
  );

  // The page or component open in its editor, from the address
  // /pages/<id> or /components/<id>.
  const pageOf = (pathname: string) => {
    const found = /^\/(?:pages|components)\/(.+)$/.exec(pathname);
    return found ? decodeURIComponent(found[1]) : null;
  };
  const [pageId, setPageId] = useState<string | null>(() =>
    pageOf(window.location.pathname),
  );
  const openPage = (id: string | null) => {
    const path = id ? `/pages/${encodeURIComponent(id)}` : "/pages";
    if (window.location.pathname !== path) window.history.pushState(null, "", path);
    setSection("pages");
    setPageId(id);
  };
  const openComponent = (id: string | null) => {
    const path = id ? `/components/${encodeURIComponent(id)}` : "/components";
    if (window.location.pathname !== path) window.history.pushState(null, "", path);
    setSection("components");
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
  // The sections of settings this person may open.
  const settings: SettingsSection[] = settingsSections.filter((name) => can(me, name));
  // A section the person may not open falls back to another of settings,
  // or to the website.
  const shown: Section =
    section === "pages" || section === "media" || section === "components"
      ? can(me, "content")
        ? section
        : "website"
      : section === "people" || section === "activity"
        ? can(me, section)
          ? section
          : (settings[0] ?? "website")
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
  // Each build that starts or ends follows a saved change: what is not
  // online yet is read again then.
  const [buildState, setBuildState] = useState("");
  // Whether a version of the site is online, for the Preview / Live switch.
  const [live, setLive] = useState(false);
  // Without a version online, there is only the preview.
  const environment: Environment = live ? chosenEnvironment : "preview";
  // The sections of the website appear once they have something in them:
  // read again after each saved change.
  const content = can(me, "content");
  const [parts, setParts] = useState<("pages" | "components" | "media")[]>([]);
  useEffect(() => {
    if (!content) return;
    Promise.all([pagesApi.list(), componentsApi.list(), mediaApi.list()]).then(
      ([pageList, componentList, mediaList]) =>
        setParts([
          ...(pageList.pages.length > 0 ? (["pages"] as const) : []),
          ...(componentList.components.length > 0 ? (["components"] as const) : []),
          ...(mediaList.media.length > 0 ? (["media"] as const) : []),
        ]),
      () => {},
    );
  }, [content, buildState]);
  // The server stopped answering: two checks in a row failed.
  const [failures, setFailures] = useState(0);
  useEffect(() => {
    const check = () =>
      fetchBuild().then(
        (status) => {
          setFailures(0);
          setRelease(status.release);
          setBuildState(`${status.state} ${status.release ?? ""}`);
          setLive(status.live);
        },
        () => {
          setFailures((count) => count + 1);
          setRelease((current) => current ?? null);
        },
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
  const siteUrl = installation ? siblingUrl(installation.site_hosts[0]) : null;

  // The preview is private: the frame enters it once with a one-time code,
  // which leaves a session of the preview in its cookie, then loads it as
  // usual.
  const [entry, setEntry] = useState<string | null>(null);
  const [entered, setEntered] = useState(false);
  useEffect(() => {
    if (!previewUrl || entered) return;
    previewEntry(previewUrl, "/").then(setEntry, () => setEntry(null));
  }, [previewUrl, entered]);
  const previewAddress = !previewUrl
    ? null
    : release === undefined
      ? undefined
      : release
        ? `${previewUrl}?build=${release}`
        : previewUrl;
  const frameUrl =
    environment === "live"
      ? siteUrl
      : entered
        ? previewAddress
        : (entry ?? undefined);

  return (
    <ChangesProvider version={buildState} enabled={can(me, "content")}>
    <NoticesProvider
      banner={
        failures >= 2
          ? {
              kind: "problem",
              title: "Connection lost",
              text: "Trying again… Changes not saved yet stay on this page.",
            }
          : null
      }
      openChat={openChat}
    >
    <div className="app" data-mobile-view={mobileView} data-section={shown}>
      <Header
        section={shown}
        siteName={siteName}
        device={device}
        onDevice={setDevice}
        onReload={() => setReloadKey((key) => key + 1)}
        onPublish={() => setPublishing(true)}
        environment={environment}
        onEnvironment={setEnvironment}
        live={live}
        mobileView={mobileView}
        onMobileView={setMobileView}
        theme={theme}
        onToggleTheme={toggleTheme}
      />
      <div className="app-body">
        <NavRail
          current={shown}
          onNavigate={navigate}
          website={content ? parts : []}
          settings={settings}
          me={me}
          onSignedOut={onSignedOut}
        />
        {/* One layout for every section: the section on the left, the chat
            on the right. The chat stays mounted across sections, so its
            conversation, terminal and stream go on without reloading. */}
        <main className="app-main" ref={main}>
          <div className="app-section">
            {shown === "people" || shown === "activity" ? (
              <SettingsPage current={shown} sections={settings} onOpen={navigate}>
                {shown === "people" ? <PeoplePage me={me} /> : <ActivityPage />}
              </SettingsPage>
            ) : shown === "pages" ? (
              pageId ? (
                <PageEditor
                  key={pageId}
                  id={pageId}
                  previewUrl={previewUrl}
                  site={installation?.site_hosts[0] ?? null}
                  onBack={() => openPage(null)}
                />
              ) : (
                <PagesPage onOpen={openPage} />
              )
            ) : shown === "components" ? (
              pageId ? (
                <ComponentEditor
                  key={pageId}
                  id={pageId}
                  onBack={() => openComponent(null)}
                />
              ) : (
                <ComponentsPage onOpen={openComponent} />
              )
            ) : shown === "media" ? (
              <MediaPage />
            ) : (
            <Preview
              url={frameUrl}
              device={device}
              reloadKey={reloadKey}
              label={environment === "live" ? "The site online" : "Preview of the site"}
              onLoad={() => {
                if (environment === "preview" && !entered && entry) setEntered(true);
              }}
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
      {publishing && (
        <PublishDialog
          site={installation?.site_hosts[0] ?? null}
          siteUrl={installation ? siblingUrl(installation.site_hosts[0]) : null}
          canPublish={can(me, "publish", "write")}
          onClose={() => setPublishing(false)}
          onReopen={() => setPublishing(true)}
        />
      )}
    </div>
    </NoticesProvider>
    </ChangesProvider>
  );
}
