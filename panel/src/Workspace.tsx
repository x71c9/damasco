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
  pages as pagesApi,
  previewEntry,
  openPreview,
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
  type MobileView,
  type PublishBlock,
  type ShownPage,
} from "./components/Header";
import { NavRail, type Section } from "./components/NavRail";
import { TabBar, type Tab } from "./components/TabBar";
import { SiteKinds } from "./components/SiteKinds";
import { PHONE_QUERY, usePhone } from "./viewport";
import { isSettings, settingsSections, type SettingsSection } from "./settings/sections";
import { SettingsPage } from "./settings/SettingsPage";
import { AssistantPage } from "./settings/AssistantPage";
import { Preview } from "./components/Preview";
import { BuildsPage } from "./builds/BuildsPage";
import { InstructionsPage } from "./settings/InstructionsPage";
import { SitesPage } from "./sites/SitesPage";
import { MediaPage } from "./media/MediaPage";
import { PageEditor } from "./pages/PageEditor";
import { PagesPage } from "./pages/PagesPage";
import { ComponentsPage } from "./parts/ComponentsPage";
import { ComponentEditor } from "./parts/ComponentEditor";
import { PublishDialog } from "./publish/PublishDialog";
import { NoDomainDialog } from "./publish/NoDomainDialog";
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
  instructions: "/settings/instructions",
  builds: "/settings/builds",
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
  // the person. Builds took the place of Activity, whose addresses lead
  // there: buttons of past conversations still name them.
  if (pathname === "/settings" || pathname === "/settings/assistant") return "assistant";
  if (["/settings/builds", "/settings/activity", "/activity"].includes(pathname)) return "builds";
  if (pathname === "/settings/instructions") return "instructions";
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
  // The page chosen in the menu of the header; the home by default.
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
  const phone = usePhone();
  const [section, setSection] = useState<Section>(() =>
    sectionOf(here()),
  );
  // Opening a site in Settings › Sites, or the list of settings on a
  // phone, changes only the address: this count shows the change.
  const [detailKey, setDetailKey] = useState(0);

  // The page or component open in its editor, from the address
  // /pages/<id> or /components/<id>.
  const pageOf = (pathname: string) => {
    const found = /^\/(?:pages|components)\/(.+)$/.exec(pathname);
    return found ? decodeURIComponent(found[1]) : null;
  };
  const [pageId, setPageId] = useState<string | null>(() =>
    pageOf(here()),
  );
  // The chat forward: on a phone it takes the screen, from any section;
  // its box takes the cursor.
  const openChat = useCallback(() => {
    setMobileView("chat");
    if (window.matchMedia(PHONE_QUERY).matches && sectionOf(here()) !== "website") {
      pushPath("/");
      setSection("website");
      setPageId(null);
    }
    window.setTimeout(() => document.getElementById("composer-input")?.focus(), 0);
  }, []);
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

  // Phones: one tab at a time. Each tab comes back where it was left: the
  // site's content at its list or at the page open in its editor, settings
  // at the section open or at their list.
  const tab: Tab = isSettings(shown) ? "settings" : shown === "website" ? mobileView : "site";
  // On a phone /settings is the list of settings, not their first section.
  const settingsList = phone && here() === "/settings";
  const lastPlace = useRef<{ site: string | null; settings: string }>({
    site: null,
    settings: "/settings",
  });
  useEffect(() => {
    const path = here();
    if (isSettings(section)) lastPlace.current.settings = path;
    else if (section !== "website") lastPlace.current.site = path;
  }, [section, pageId, detailKey]);
  const goPath = (path: string) => {
    if (path !== here()) pushPath(path);
    setSection(sectionOf(path));
    setPageId(pageOf(path));
    setDetailKey((key) => key + 1);
  };

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

  // The latest build of the site, as the server reports it.
  const [release, setRelease] = useState<string | null | undefined>(
    undefined,
  );
  // Each build that starts or ends follows a saved change: what is not
  // online yet is read again then.
  const [buildState, setBuildState] = useState("");
  // Whether a version of the site is online, for the button Live.
  const [live, setLive] = useState(false);
  // The sections of the website appear once they have something in them,
  // read again after each saved change. Media is always there: the first
  // picture is uploaded in it.
  const [parts, setParts] = useState<("pages" | "components" | "media")[]>([]);
  useEffect(() => {
    Promise.all([pagesApi.list(), componentsApi.list()]).then(
      ([pageList, componentList]) =>
        setParts([
          ...(pageList.pages.length > 0 ? (["pages"] as const) : []),
          ...(componentList.components.length > 0 ? (["components"] as const) : []),
          "media",
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

  // The page the preview shows, as the preview itself tells it (the
  // script `/_damasco/frame.js` of the server): a reload opens it again,
  // instead of the page chosen in the menu. Pages whose own rules keep the
  // script out are reloaded at the last page known.
  const framePath = useRef("/");
  const previewUrl = siblingUrl(site.preview_host);
  useEffect(() => {
    const origin = new URL(previewUrl).origin;
    const onMessage = (event: MessageEvent) => {
      if (event.origin !== origin) return;
      const data = event.data as { damasco?: unknown; path?: unknown } | null;
      if (data?.damasco !== "preview-page" || typeof data.path !== "string") return;
      if (!data.path.startsWith("/")) return;
      // The address of the build is the panel's, not the page's.
      const url = new URL(data.path, origin);
      url.searchParams.delete("build");
      framePath.current = `${url.pathname}${url.search}${url.hash}`;
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [previewUrl]);

  // What the frame loads: a build and a page. Its address names the build,
  // so the browser never shows a copy of an older one from its cache.
  // Each change of the key loads it again.
  const [frame, setFrame] = useState<{
    release: string | null | undefined;
    path: string;
    key: number;
  }>({ release: undefined, path: "/", key: 0 });
  // Asked by the person (reload, a page of the menu): the latest build, even during an answer.
  const reloadFrame = (path?: string) => {
    if (path !== undefined) framePath.current = path;
    setFrame((current) => ({
      release,
      path: framePath.current,
      key: current.key + 1,
    }));
  };
  // A new build reloads the preview at the page it shows, except during an
  // answer of the assistant on this site: the preview loads once, when the
  // answer ends, with the build that includes every change.
  const answering = busy?.site_id === site.id && busy.kind === "turn";
  useEffect(() => {
    setFrame((current) =>
      current.release === release || (answering && current.release !== undefined)
        ? current
        : { release, path: framePath.current, key: current.key + 1 },
    );
  }, [release, answering]);
  // The end of an answer asks for the latest build at once, without
  // waiting for the next check.
  const showLatestBuild = () => {
    fetchBuild().then(
      (status) => setRelease(status.release),
      () => {},
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

  const siteUrl = site.site_hosts[0] ? siblingUrl(site.site_hosts[0]) : null;
  // Publish waits for a domain, and while the assistant or the terminal
  // works on the site; the server refuses it then too.
  const publishBlock: PublishBlock | null =
    site.site_hosts.length === 0
      ? "domain"
      : busy?.site_id === site.id
        ? busy.kind
        : null;

  // The preview is private: the frame enters it once with a one-time code,
  // which leaves a session of the preview in its cookie, then loads it as
  // usual.
  const [entry, setEntry] = useState<string | null>(null);
  const [entered, setEntered] = useState(false);
  useEffect(() => {
    if (!previewUrl || entered) return;
    previewEntry(previewUrl, "/").then(setEntry, () => setEntry(null));
  }, [previewUrl, entered]);
  const pageAt = (base: string, build?: string | null) => {
    const url = new URL(frame.path, base);
    if (build) url.searchParams.set("build", build);
    return url.toString();
  };
  const previewAddress =
    frame.release === undefined ? undefined : pageAt(previewUrl, frame.release);
  const frameUrl = entered ? previewAddress : (entry ?? undefined);
  // Both open in a new tab at the page the preview shows; Live only once
  // the site has a domain and a version online.
  const openLive =
    siteUrl && live
      ? () => window.open(new URL(framePath.current, siteUrl).toString(), "_blank", "noopener")
      : null;

  const goTab = (next: Tab) => {
    if (next === "chat" || next === "preview") {
      setMobileView(next);
      if (section !== "website") navigate("website");
      return;
    }
    // The tab already open goes back to its start: the list of its kind,
    // or the list of settings.
    if (next === tab) {
      goPath(next === "settings" ? "/settings" : `/${here().split("/")[1]}`);
      return;
    }
    goPath(
      next === "settings"
        ? lastPlace.current.settings
        : (lastPlace.current.site ?? (parts.includes("pages") ? "/pages" : "/media")),
    );
  };
  // The kinds of content of the site, above their lists on a phone.
  const siteKinds = phone && (shown === "pages" || shown === "components" || shown === "media") && !pageId;

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
        canCreateSite={installation !== null && !installation.single_site}
        device={device}
        onDevice={setDevice}
        onReload={() => reloadFrame()}
        onPublish={() => setPublishing(true)}
        publishBlock={publishBlock}
        onOpenPreview={() => openPreview(previewUrl, framePath.current)}
        onOpenLive={openLive}
        mobileView={mobileView}
        onMobileView={setMobileView}
        theme={theme}
        onToggleTheme={toggleTheme}
        onHome={() => navigate('website')}
        page={shownPage}
        onPage={(page) => {
          setShownPage(page);
          reloadFrame(page?.address ?? "/");
        }}
      />
      <div className="app-body">
        {!phone && (
          <NavRail
            current={shown}
            onNavigate={navigate}
            website={parts}
            settings={settings}
            onSignedOut={onSignedOut}
          />
        )}
        {/* One layout for every section: the section on the left, the chat
            on the right. The chat stays mounted across sections, so its
            conversation, terminal and stream go on without reloading. */}
        <main className="app-main" ref={main}>
          <div className="app-section">
            {siteKinds && (
              <SiteKinds
                kinds={parts.includes(shown as "pages" | "components" | "media") ? parts : [...parts, shown as "pages" | "components" | "media"]}
                current={shown as "pages" | "components" | "media"}
                onOpen={(kind) => goPath(paths[kind])}
              />
            )}
            {isSettings(shown) ? (
              <SettingsPage
                current={shown}
                sections={settings}
                onOpen={(next) => goPath(paths[next])}
                version={installation?.version}
                phone={phone}
                list={settingsList}
                onList={() => goPath("/settings")}
                theme={theme}
                onToggleTheme={toggleTheme}
                onSignedOut={onSignedOut}
              >
                {shown === "assistant" ? (
                  <AssistantPage account={account} onAccount={setAccount} />
                ) : shown === "instructions" ? (
                  <InstructionsPage site={site} installation={installation} />
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
                  <BuildsPage site={site.name} version={buildState} />
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
              device={phone ? "computer" : device}
              reloadKey={frame.key}
              label="Preview of the site"
              onLoad={() => {
                if (!entered && entry) setEntered(true);
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
      {phone && <TabBar current={tab} onTab={goTab} />}
      {publishing && publishBlock === "domain" ? (
        <NoDomainDialog slug={site.slug} onClose={() => setPublishing(false)} />
      ) : publishing ? (
        <PublishDialog
          site={site.site_hosts[0] ?? site.name}
          siteUrl={siteUrl}
          onClose={() => setPublishing(false)}
          onReopen={() => setPublishing(true)}
          waitingFor={buildingElsewhere}
        />
      ) : null}
    </div>
    </NoticesProvider>
    </ChangesProvider>
  );
}
