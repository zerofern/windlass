// Live updates and local times for the Windlass dashboard. Pages work without it.
(() => {
  "use strict";

  const pad = (value, width) => String(value).padStart(width, "0");

  function localise(root) {
    for (const element of root.querySelectorAll("time[datetime]")) {
      const date = new Date(element.dateTime);
      if (Number.isNaN(date.getTime())) continue;
      element.title = date.toString();
      element.textContent =
        element.dataset.style === "clock"
          ? `${date.toLocaleTimeString([], { hour12: false })}.${pad(date.getMilliseconds(), 3)}`
          : date.toLocaleString();
    }
  }

  function replaceLive(html) {
    const template = document.createElement("template");
    template.innerHTML = html;
    for (const fresh of template.content.querySelectorAll("[data-live]")) {
      const current = document.querySelector(`[data-live="${fresh.dataset.live}"]`);
      if (current) current.replaceWith(fresh);
    }
    localise(document);
  }

  function connect() {
    const indicator = document.getElementById("connection");
    const banner = document.getElementById("disconnected");
    const source = new EventSource(`/live?page=${encodeURIComponent(location.pathname)}`);
    source.addEventListener("open", () => {
      document.body.classList.remove("disconnected");
      indicator.textContent = "Live";
      indicator.classList.add("live");
      banner.hidden = true;
    });
    source.addEventListener("error", () => {
      if (document.body.classList.contains("disconnected")) return;
      document.body.classList.add("disconnected");
      indicator.textContent = "Disconnected";
      indicator.classList.remove("live");
      banner.textContent =
        `Lost connection to Windlass at ${new Date().toLocaleTimeString([], { hour12: false })}. ` +
        "Everything below is from before then. Reconnecting…";
      banner.hidden = false;
    });
    source.addEventListener("live", (event) => replaceLive(event.data));
  }

  function convertFilterTimes() {
    const filter = document.getElementById("history-filter");
    if (!filter) return;
    filter.addEventListener("submit", () => {
      // An empty field keeps the current filter; "Clear filters" removes it.
      for (const name of ["from", "to"]) {
        const local = filter.elements[`${name}_local`];
        if (local.value) filter.elements[name].value = new Date(local.value).toISOString();
        local.disabled = true;
      }
    });
  }

  document.addEventListener("DOMContentLoaded", () => {
    localise(document);
    convertFilterTimes();
    connect();
  });
})();
