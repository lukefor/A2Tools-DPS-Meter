// Settings → What's new (#127): the last releases' notes, newest first, in
// the meter's language where the release has it, else English.
//
// The backend fetches them (`release_notes`: cdn.a2tools.app's
// changelog-v2.json, signature-checked like the update manifest, made from
// the GitHub releases by tools/changelog.mjs). The notes are the releases'
// Markdown; only what they use is drawn (headings, bold lines, bullets,
// **bold**, `code`, links), and drawn as text, never as HTML.
(() => {
  const RELEASES_URL = "https://github.com/taengu/A2Tools-DPS-Meter/releases";
  // Links in the notes open only to these.
  const LINK_HOSTS = ["github.com", "a2tools.app", "discord.gg"];

  const section = document.querySelector("#settingsSection-changelog");
  if (!section) return;
  const list = section.querySelector(".changelogList");
  const status = section.querySelector(".changelogStatus");
  const allLink = section.querySelector(".changelogAllLink");
  const invoke = window.__TAURI__?.core?.invoke;
  const t = (key, fallback, vars) => window.i18n?.t?.(key, fallback, vars) ?? fallback;

  let releases = null;
  let installed = "";
  let loading = false;

  const openLink = (url) => window.javaBridge?.openBrowser?.(url);
  allLink?.addEventListener("click", () => openLink(RELEASES_URL));

  const parts = (v) => String(v).replace(/^v/i, "").split(/[.-]/).map((n) => parseInt(n, 10) || 0);
  const compare = (a, b) => {
    const [x, y] = [parts(a), parts(b)];
    for (let i = 0; i < Math.max(x.length, y.length); i++) {
      if ((x[i] || 0) !== (y[i] || 0)) return (x[i] || 0) - (y[i] || 0);
    }
    return 0;
  };

  const allowedLink = (url) => {
    try {
      const u = new URL(url);
      return u.protocol === "https:" && LINK_HOSTS.some((h) => u.hostname === h || u.hostname.endsWith(`.${h}`));
    } catch {
      return false;
    }
  };

  // **bold**, `code` and [text](url) within a line.
  const inline = (parent, text) => {
    const pattern = /\*\*(.+?)\*\*|`([^`]+)`|\[([^\]]+)\]\(([^)\s]+)\)/g;
    let at = 0;
    for (const m of text.matchAll(pattern)) {
      if (m.index > at) parent.append(text.slice(at, m.index));
      if (m[1] !== undefined) {
        const b = document.createElement("strong");
        inline(b, m[1]);
        parent.append(b);
      } else if (m[2] !== undefined) {
        const c = document.createElement("code");
        c.textContent = m[2];
        parent.append(c);
      } else if (allowedLink(m[4])) {
        const a = document.createElement("button");
        a.type = "button";
        a.className = "linkButton";
        a.textContent = m[3];
        a.addEventListener("click", () => openLink(m[4]));
        parent.append(a);
      } else {
        parent.append(m[3]);
      }
      at = m.index + m[0].length;
    }
    if (at < text.length) parent.append(text.slice(at));
  };

  const renderNotes = (markdown) => {
    const body = document.createElement("div");
    body.className = "changelogNotes";
    // The open lists by depth, so an indented bullet goes in the one above.
    let lists = [];
    for (const line of String(markdown).split(/\r?\n/)) {
      const bullet = line.match(/^(\s*)[-*]\s+(.*)$/);
      if (bullet) {
        const depth = Math.min(Math.floor(bullet[1].length / 2), 3);
        while (lists.length > depth + 1) lists.pop();
        if (lists.length < depth + 1) {
          const ul = document.createElement("ul");
          const host = lists.length ? lists[lists.length - 1].lastElementChild || lists[lists.length - 1] : body;
          host.append(ul);
          lists.push(ul);
        }
        const li = document.createElement("li");
        inline(li, bullet[2]);
        lists[lists.length - 1].append(li);
        continue;
      }
      lists = [];
      const text = line.trim();
      if (!text) continue;
      const heading = text.match(/^#{1,6}\s+(.*)$/);
      const el = document.createElement(heading ? "h4" : "p");
      // A line that is only bold ("**Fixes**") heads what follows.
      if (heading || /^\*\*[^*]+\*\*(\s*\(.*\))?$/.test(text)) el.className = "changelogHeading";
      inline(el, heading ? heading[1] : text);
      body.append(el);
    }
    return body;
  };

  const render = () => {
    if (!list || !releases) return;
    const lang = window.i18n?.getLanguage?.() || "en";
    list.replaceChildren();
    releases.forEach((release, index) => {
      const notes = release.notes?.[lang] || release.notes?.en;
      if (!notes) return;
      const isInstalled = installed && compare(release.version, installed) === 0;
      const isNewer = installed && compare(release.version, installed) > 0;
      const card = document.createElement("details");
      card.className = "changelogRelease";
      card.classList.toggle("isInstalled", !!isInstalled);
      card.classList.toggle("isNewer", !!isNewer);
      // The newest, and any you haven't installed yet, start open.
      card.open = index === 0 || !!isNewer;
      const summary = document.createElement("summary");
      const version = document.createElement("span");
      version.className = "changelogVersion";
      version.textContent = `v${release.version}`;
      summary.append(version);
      if (isInstalled || isNewer) {
        const badge = document.createElement("span");
        badge.className = "changelogBadge";
        badge.textContent = isInstalled
          ? t("changelog.installed", "Installed")
          : t("changelog.available", "Update available");
        summary.append(badge);
      }
      if (release.date) {
        const date = document.createElement("span");
        date.className = "changelogDate";
        const d = new Date(`${release.date}T12:00:00Z`);
        date.textContent = Number.isNaN(d.getTime())
          ? release.date
          : d.toLocaleDateString(document.documentElement.lang || undefined, { year: "numeric", month: "short", day: "numeric" });
        summary.append(date);
      }
      card.append(summary, renderNotes(notes));
      list.append(card);
    });
  };

  const load = async () => {
    if (releases || loading || !invoke) return;
    loading = true;
    status.textContent = t("changelog.loading", "Loading the release notes…");
    try {
      installed = (await invoke("get_app_version").catch(() => "")) || "";
      const data = await invoke("release_notes");
      releases = Array.isArray(data?.releases) ? data.releases : [];
      status.textContent = releases.length ? "" : t("changelog.none", "No release notes yet.");
      render();
    } catch (error) {
      releases = null;
      status.textContent = t(
        "changelog.failed",
        "Couldn't load the release notes. They are also on GitHub, below.",
      );
      window.uiDebug?.log?.("release notes", { error: String(error) });
    } finally {
      loading = false;
    }
  };

  document.addEventListener("settingsSectionShown", (event) => {
    if (event.detail === "changelog") load();
  });
  if (!section.hidden) load();
  window.i18n?.onChange?.(() => render());
})();
