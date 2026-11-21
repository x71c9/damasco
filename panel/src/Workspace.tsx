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
  components as componentsApi,
  media as mediaApi,
  pages as pagesApi,
  previewEntry,
  fetchBuild,
  fetchInstallation,
  siblingUrl,
  type AgentAccount,
  type Installation,
  type SiteInfo,
  type Me,
} from "./api";
import { ChangesProvider } from "./changes/changes";
import { NoticesProvider } from "./notices/Notices";
import { Chat } from "./components/Chat";
import { ConnectAssistant } from "./components/ConnectAssistant";
import {
  Header,
  type Device,
  type Environment,
  type MobileView,
  type ShownPage,
} from "./components/Header";
import { NavRail, type Section } from "./components/NavRail";
import { settingsSections, type SettingsSection } from "./settings/sections";
import { SettingsPage } from "./settings/SettingsPage";
import { AssistantPage } from "./settings/AssistantPage";
import { Preview } from "./components/Preview";
import { ActivityPage } from "./activity/ActivityPage";
import { SitesPage } from "./sites/SitesPage";
import { MediaPage } from "./media/MediaPage";
import { PageEditor } from "./pages/PageEditor";
import { PagesPage } from "./pages/PagesPage";
import { ComponentsPage } from "./parts/ComponentsPage";
import { ComponentEditor } from "./parts/ComponentEditor";
import { PublishDialog } from "./publish/PublishDialog";
import { currentTheme, setTheme, type Theme } from "./theme";
import { here, pushPath } from "./navigation";
import "./Workspace.css";

const CHAT_MIN = 360;
/** The chat takes at most this share of the space it shares with the
 * preview. */
const CHAT_SHARE = 0.75;
const CHAT_WIDTH_KEY = "damasco.chat.width";

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
  assistant: "/settings/assistant",
  sites: "/settings/sites",
  activity: "/settings/activity",
  pages: "/pages",
  components: "/components",
  media: "/media",
};

/** The site open in Settings › Sites, from /settings/sites/<slug>. */
function detailOf(pathname: string): string | null {
  const found = /^\/settings\/sites\/([^/]+)/.exec(pathname);
  return found ? decodeURIComponent(found[1]) : null;
}

function sectionOf(pathname: string): Section {
  // Settings open on their first section, which the workspace picks for
  // the person; /activity is the address of before.
  if (pathname === "/settings" || pathname === "/settings/assistant") return "assistant";
  if (pathname === "/settings/activity" || pathname === "/activity") return "activity";
  if (pathname === "/settings/sites" || pathname.startsWith("/settings/sites/")) return "sites";
  if (pathname === "/pages" || pathname.startsWith("/pages/")) return "pages";
  if (pathname === "/components" || pathname.startsWith("/components/")) return "components";
  if (pathname === "/media") return "media";
  return "website";
}

/** The signed-in panel: header, sections, preview and chat. */
export function Workspace({
  me,
  site,
  sites,
  onSitesChanged,
  onSignedOut,
}: {
  me: Me;
  /** The site the panel works on, and every site. */
  site: SiteInfo;
  sites: SiteInfo[];
  /** The sites or their addresses changed: the list is read again. */
  onSitesChanged: () => void;
  onSignedOut: () => void;
}) {
  const [installation, setInstallation] = useState<Installation | null>(null);
  const [device, setDevice] = useState<Device>("computer");
  const [reloadKey, setReloadKey] = useState(0);
  // The page the preview opens, by its address; the home by default.
  const [shownPage, setShownPage] = useState<ShownPage | null>(null);
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
    sectionOf(here()),
  );
  // Opening a site in Settings › Sites changes only the address: this
  // count shows the change.
  const [, setDetailKey] = useState(0);

  // The page or component open in its editor, from the address
  // /pages/<id> or /components/<id>.
  const pageOf = (pathname: string) => {
    const found = /^\/(?:pages|components)\/(.+)$/.exec(pathname);
    return found ? decodeURIComponent(found[1]) : null;
  };
  const [pageId, setPageId] = useState<string | null>(() =>
    pageOf(here()),
  );
  const openPage = (id: string | null) => {
    const path = id ? `/pages/${encodeURIComponent(id)}` : "/pages";
    pushPath(path);
    setSection("pages");
    setPageId(id);
  };
  const openComponent = (id: string | null) => {
    const path = id ? `/components/${encodeURIComponent(id)}` : "/components";
    pushPath(path);
    setSection("components");
    setPageId(id);
  };

  // Each section has its own address, so reload and the back button work.
  useEffect(() => {
    const onPop = () => {
      setSection(sectionOf(here()));
      setPageId(pageOf(here()));
      setDetailKey((key) => key + 1);
    };
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);
  const navigate = (next: Section) => {
    if (next !== section || pageId) pushPath(paths[next]);
    setSection(next);
    setPageId(null);
  };
  const settings: SettingsSection[] = [...settingsSections];
  const shown: Section = section;

  useEffect(() => {
    fetchInstallation().then(setInstallation, (error: unknown) =>
      console.error(error),
    );
  }, []);

  // The assistant works on one site at a time: which one, from the
  // server's events, so the chat can say it is at work on another.
  const [busy, setBusy] = useState<{ site_id: string; kind: "turn" | "terminal" } | null>(null);
  // The site whose build runs: builds go one at a time across the sites.
  const [building, setBuilding] = useState<string | null>(null);
  useEffect(() => {
    const source = new EventSource("/api/events");
    source.addEventListener("busy", (event) => {
      const data = JSON.parse((event as MessageEvent<string>).data) as {
        agent: { site_id: string; kind: "turn" | "terminal" } | null;
        build: string | null;
      };
      setBusy(data.agent);
      setBuilding(data.build);
    });
    return () => source.close();
  }, []);
  const busySite = busy && busy.site_id !== site.id ? sites.find((other) => other.id === busy.site_id) : undefined;
  const busyElsewhere = busy && busySite ? { ...busySite, kind: busy.kind } : null;
  const buildingElsewhere =
    building && building !== site.id ? (sites.find((other) => other.id === building)?.name ?? null) : null;

  // The chat needs the assistant's account.
  const [account, setAccount] = useState<AgentAccount | null>(null);
  useEffect(() => {
    agent.account().then(setAccount, () => {});
  }, []);

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
  const [parts, setParts] = useState<("pages" | "components" | "media")[]>([]);
  useEffect(() => {
    Promise.all([pagesApi.list(), componentsApi.list(), mediaApi.list()]).then(
      ([pageList, componentList, mediaList]) =>
        setParts([
          ...(pageList.pages.length > 0 ? (["pages"] as const) : []),
          ...(componentList.components.length > 0 ? (["components"] as const) : []),
          ...(mediaList.media.length > 0 ? (["media"] as const) : []),
        ]),
      () => {},
    );
  }, [buildState]);
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

  const previewUrl = siblingUrl(site.preview_host);
  const siteUrl = site.site_hosts[0] ? siblingUrl(site.site_hosts[0]) : null;

  // The preview is private: the frame enters it once with a one-time code,
  // which leaves a session of the preview in its cookie, then loads it as
  // usual.
  const [entry, setEntry] = useState<string | null>(null);
  const [entered, setEntered] = useState(false);
  useEffect(() => {
    if (!previewUrl || entered) return;
    previewEntry(previewUrl, "/").then(setEntry, () => setEntry(null));
  }, [previewUrl, entered]);
  const path = (shownPage?.address ?? "/").replace(/^\//, "");
  const previewAddress = !previewUrl
    ? null
    : release === undefined
      ? undefined
      : release
        ? `${previewUrl}${path}?build=${release}`
        : `${previewUrl}${path}`;
  const frameUrl =
    environment === "live"
      ? siteUrl && `${siteUrl}${path}`
      : entered
        ? previewAddress
        : (entry ?? undefined);

  return (
    <ChangesProvider version={buildState} enabled>
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
        site={site}
        sites={sites}
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
        onHome={() => navigate('website')}
        page={shownPage}
        onPage={setShownPage}
      />
      <div className="app-body">
        <NavRail
          current={shown}
          onNavigate={navigate}
          website={parts}
          settings={settings}
          onSignedOut={onSignedOut}
        />
        {/* One layout for every section: the section on the left, the chat
            on the right. The chat stays mounted across sections, so its
            conversation, terminal and stream go on without reloading. */}
        <main className="app-main" ref={main}>
          <div className="app-section">
            {shown === "assistant" || shown === "sites" || shown === "activity" ? (
              <SettingsPage
                current={shown}
                sections={settings}
                onOpen={navigate}
                version={installation?.version}
              >
                {shown === "assistant" ? (
                  <AssistantPage account={account} onAccount={setAccount} />
                ) : shown === "sites" ? (
                  <SitesPage
                    list={sites}
                    current={site}
                    open={detailOf(here())}
                    onOpen={(slug) => {
                      pushPath(slug ? `/settings/sites/${encodeURIComponent(slug)}` : "/settings/sites");
                      setDetailKey((key) => key + 1);
                    }}
                    installation={installation}
                    onChanged={onSitesChanged}
                  />
                ) : (
                  <ActivityPage />
                )}
              </SettingsPage>
            ) : shown === "pages" ? (
              pageId ? (
                <PageEditor
                  key={pageId}
                  id={pageId}
                  previewUrl={previewUrl}
                  site={site.site_hosts[0] ?? site.name}
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
                      busyElsewhere={busyElsewhere}
                    />
                  ) : (
                    <ConnectAssistant
                      account={account}
                      onConnected={setAccount}
                    />
                  )}
                </div>
              </>
        </main>
      </div>
      {publishing && (
        <PublishDialog
          site={site.site_hosts[0] ?? site.name}
          siteUrl={siteUrl}
          onClose={() => setPublishing(false)}
          onReopen={() => setPublishing(true)}
          waitingFor={buildingElsewhere}
        />
      )}
    </div>
    </NoticesProvider>
    </ChangesProvider>
  );
}
